//! Process-owned stderr drainage with a bounded, already-redacted tail.
//!
//! Ownership and the 250 ms drain grace.
//! Copyright OpenAI. Licensed under the Apache License, Version 2.0.

use super::stderr_tail::Tail;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

const DRAIN: Duration = Duration::from_millis(250);

#[derive(Clone, Default)]
pub(crate) struct Capture(Arc<Mutex<Tail>>);

impl Capture {
    fn with<T>(&self, read: impl FnOnce(&mut Tail) -> T) -> T {
        match self.0.lock() {
            Ok(mut tail) => read(&mut tail),
            Err(poisoned) => read(&mut poisoned.into_inner()),
        }
    }

    pub(crate) fn protect(&self, value: &str) {
        self.with(|tail| tail.protect(value));
    }
    pub(crate) fn sanitise(&self, value: &str) -> String {
        self.with(|tail| tail.sanitise(value))
    }
    pub(crate) fn text(&self) -> String {
        self.with(|tail| tail.text())
    }
}

pub(crate) struct Reader {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
    capture: Capture,
}

impl Reader {
    pub(crate) fn start<R: AsyncRead + Unpin + Send + 'static>(
        mut pipe: R,
        capture: Capture,
    ) -> Self {
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let output = capture.clone();
        let task = tokio::spawn(async move {
            let drain = async {
                let mut chunk = [0u8; 1_024];
                loop {
                    match pipe.read(&mut chunk).await {
                        Ok(0) => break,
                        Err(_) => {
                            output.with(|tail| tail.push(b" stderr reader failed"));
                            break;
                        }
                        Ok(count) => output.with(|tail| tail.push(&chunk[..count])),
                    }
                }
            };
            tokio::pin!(drain);
            tokio::select! {
                () = &mut drain => {},
                _ = stopped => { let _ = tokio::time::timeout(DRAIN, &mut drain).await; },
            }
            output.with(Tail::finish);
        });
        Self {
            stop: Some(stop),
            task: Some(task),
            capture,
        }
    }

    /// Startup failure can join the reader. Drop also stops it; cancellation
    /// cannot leave a reader waiting indefinitely on an escaped descendant.
    pub(crate) async fn finish(mut self) -> String {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
        self.capture.text()
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        // As in upstream, the worker owns only its pipe and bounded tail after
        // this handle drops. Its stop branch retires both within DRAIN.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn unterminated_stderr_drains_and_retirement_does_not_need_eof(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (pipe, mut writer) = tokio::io::duplex(2_048);
        let capture = Capture::default();
        let reader = Reader::start(pipe, capture.clone());
        writer.write_all(&vec![b'x'; 10_000]).await?;
        writer.write_all(b" fixture process exited").await?;
        let text = tokio::time::timeout(Duration::from_secs(2), reader.finish()).await?;
        assert!(text.ends_with("fixture process exited"));
        assert!(text.len() <= 4_096);
        assert!(writer.write_all(b"descendant").await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_releases_the_pipe_after_a_bounded_drain(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (pipe, mut writer) = tokio::io::duplex(2_048);
        let reader = Reader::start(pipe, Capture::default());
        let finished = reader.task.as_ref().ok_or("reader task")?.abort_handle();
        drop(reader);
        tokio::time::timeout(Duration::from_secs(2), async {
            while !finished.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        assert!(writer.write_all(b"still held").await.is_err());
        Ok(())
    }
}
