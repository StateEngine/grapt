//! Bounded lossy output; the sole detached writer never holds reply custody.
use std::io::{self, Write};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc::{sync_channel, SyncSender},
    Arc,
};
const CAPACITY: usize = 128;
const LINE_BYTES: usize = 8192;
#[derive(Debug)]
pub(super) struct LossyStderr {
    sender: Option<SyncSender<Vec<u8>>>,
    lost: Arc<AtomicU64>,
}
impl LossyStderr {
    pub(super) fn stderr() -> Self {
        let lost = Arc::new(AtomicU64::new(0));
        // Duplicate the OS handle: a stopped reader cannot acquire and retain
        // Rust's global stderr lock used by unrelated existing diagnostics.
        match stderr_file().and_then(|file| Self::start(file, Arc::clone(&lost))) {
            Ok(output) => output,
            Err(_) => Self { sender: None, lost },
        }
    }
    fn start(mut sink: impl Write + Send + 'static, lost: Arc<AtomicU64>) -> io::Result<Self> {
        let (sender, receiver) = sync_channel::<Vec<u8>>(CAPACITY);
        let worker_loss = Arc::clone(&lost);
        // One thread per service/connection, one in-flight line plus 128 queued
        // lines, each <=8192 B. Never join it on execution or process shutdown.
        // A permanently stopped pipe retains only this bounded writer until exit.
        let _worker = std::thread::Builder::new()
            .name("acp-stderr".into())
            .spawn(move || {
                let mut reported = 0;
                while let Ok(line) = receiver.recv() {
                    if sink.write_all(&line).is_err() {
                        increment(&worker_loss);
                        break;
                    }
                    let count = worker_loss.load(Ordering::Relaxed);
                    if count != reported {
                        if writeln!(sink, "GRAPT_PROVIDER_TAIL_LOSS acp_count_at_least={count}")
                            .is_err()
                        {
                            increment(&worker_loss);
                            break;
                        }
                        reported = count;
                    }
                }
            })?;
        Ok(Self {
            sender: Some(sender),
            lost,
        })
    }
    fn bytes(&self, line: &[u8]) {
        if line.len() > LINE_BYTES
            || self
                .sender
                .as_ref()
                .is_none_or(|sender| sender.try_send(line.to_vec()).is_err())
        {
            increment(&self.lost);
        }
    }
}
fn increment(lost: &AtomicU64) {
    let _ = lost.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
        Some(old.saturating_add(1))
    });
}
fn stderr_file() -> io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        Ok(std::fs::File::from(
            std::io::stderr().as_fd().try_clone_to_owned()?,
        ))
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsHandle;
        Ok(std::fs::File::from(
            std::io::stderr().as_handle().try_clone_to_owned()?,
        ))
    }
}

impl tokio::io::AsyncWrite for LossyStderr {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        self.bytes(bytes);
        std::task::Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tokio::io::AsyncWriteExt;
    #[tokio::test]
    async fn stopped_native_pipe_does_not_hold_tokio_shutdown_or_flush(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (mut reader, writer) = std::io::pipe()?;
        let lost = Arc::new(AtomicU64::new(0));
        let mut output = LossyStderr::start(writer, Arc::clone(&lost))?;
        tokio::time::timeout(std::time::Duration::from_millis(250), async {
            for _ in 0..512 {
                output.write_all(&[b'x'; LINE_BYTES]).await?;
            }
            output.flush().await?;
            output.shutdown().await
        })
        .await??;
        assert!(
            lost.load(Ordering::Relaxed) > 0,
            "native output loss went unreported"
        );
        drop(output);
        let drain = tokio::task::spawn_blocking(move || {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).map(|_| bytes)
        });
        let bytes = tokio::time::timeout(std::time::Duration::from_secs(3), drain).await???;
        assert!(String::from_utf8_lossy(&bytes)
            .contains("GRAPT_PROVIDER_TAIL_LOSS acp_count_at_least="));
        Ok(())
    }
}
