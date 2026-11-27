//! Bounded lossy output; the sole detached writer never holds product custody.
use std::io::{self, Write};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc::{sync_channel, SyncSender},
    Arc,
};
const CAPACITY: usize = 128;
const LINE_BYTES: usize = 8192;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    Queued,
    Dropped,
}
#[derive(Debug)]
pub struct LossyOutput {
    sender: Option<SyncSender<Vec<u8>>>,
    lost: Arc<AtomicU64>,
}
impl LossyOutput {
    pub fn disabled(lost: Arc<AtomicU64>) -> Self {
        Self { sender: None, lost }
    }

    pub fn stderr(lost: Arc<AtomicU64>, loss_field: &'static str) -> Self {
        // Duplicate the OS handle: a stopped reader cannot acquire and retain
        // Rust's global stderr lock used by unrelated existing diagnostics.
        match stderr_file().and_then(|file| Self::start(file, Arc::clone(&lost), loss_field)) {
            Ok(output) => output,
            Err(_) => Self { sender: None, lost },
        }
    }
    pub fn start(
        mut sink: impl Write + Send + 'static,
        lost: Arc<AtomicU64>,
        loss_field: &'static str,
    ) -> io::Result<Self> {
        let (sender, receiver) = sync_channel::<Vec<u8>>(CAPACITY);
        let worker_loss = Arc::clone(&lost);
        // One thread per service/connection, one in-flight line plus 128 queued
        // lines, each <=8192 B. Never join it on execution or process shutdown.
        // A permanently stopped pipe retains only this bounded writer until exit.
        let _worker = std::thread::Builder::new()
            .name("diagnostic-stderr".into())
            .spawn(move || {
                let mut reported = 0;
                while let Ok(line) = receiver.recv() {
                    if sink.write_all(&line).is_err() {
                        increment(&worker_loss);
                        // Drop this line, not the diagnostic writer. A later
                        // line can succeed after a temporary sink failure.
                        continue;
                    }
                    let count = worker_loss.load(Ordering::Relaxed);
                    if count != reported {
                        if writeln!(sink, "{loss_field}={count}").is_err() {
                            increment(&worker_loss);
                        } else {
                            // A failed notice remains owed on the next
                            // successful write; never claim it was emitted.
                            reported = count;
                        }
                    }
                }
            })?;
        Ok(Self {
            sender: Some(sender),
            lost,
        })
    }
    pub fn lost_at_least(&self) -> u64 {
        self.lost.load(Ordering::Relaxed)
    }
    pub fn send(&self, line: String) -> Offer {
        if line.len() > LINE_BYTES
            || self
                .sender
                .as_ref()
                .is_none_or(|sender| sender.try_send(line.into_bytes()).is_err())
        {
            increment(&self.lost);
            return Offer::Dropped;
        }
        Offer::Queued
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

#[cfg(test)]
mod tests;
