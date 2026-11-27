//! Diagnostic writes never run on the ACP stdout worker.
use super::timing::AcpTiming;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
const CAPACITY: usize = 128;
const MAXIMUM: usize = 8192;
pub(super) struct DiagnosticOutput {
    sender: mpsc::Sender<String>,
    worker: tokio::task::JoinHandle<()>,
    observer: Arc<AcpTiming>,
}
impl DiagnosticOutput {
    pub(super) fn start(
        mut sink: impl AsyncWrite + Unpin + Send + 'static,
        observer: Arc<AcpTiming>,
    ) -> Self {
        let (sender, mut receiver) = mpsc::channel::<String>(CAPACITY);
        let losses = Arc::clone(&observer);
        let worker = tokio::spawn(async move {
            let mut reported = 0;
            while let Some(line) = receiver.recv().await {
                if !matches!(
                    tokio::time::timeout(Duration::from_secs(30), sink.write_all(line.as_bytes()))
                        .await,
                    Ok(Ok(()))
                ) {
                    losses.lose();
                }
                let lost = losses.lost_at_least();
                if lost != reported {
                    let line = format!("GRAPT_PROVIDER_TAIL_LOSS acp_count_at_least={lost}\n");
                    if !matches!(
                        tokio::time::timeout(
                            Duration::from_secs(30),
                            sink.write_all(line.as_bytes())
                        )
                        .await,
                        Ok(Ok(()))
                    ) {
                        losses.lose();
                    }
                    reported = lost;
                }
            }
        });
        Self {
            sender,
            worker,
            observer,
        }
    }
    pub(super) fn send(&self, mut line: String) {
        let mut end = line.len().min(MAXIMUM - 1);
        while !line.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        line.truncate(end);
        if !line.ends_with('\n') {
            line.push('\n');
        }
        if self.sender.try_send(line).is_err() {
            self.observer.lose();
        }
    }
}
impl Drop for DiagnosticOutput {
    fn drop(&mut self) {
        // Cancellation never waits on a diagnostic sink. Native stderr is
        // itself a nonblocking bounded adapter, not Tokio's blocking stdio pool.
        self.worker.abort();
        // Queued lines canceled at connection end are also explicit loss.
        for _ in 0..self
            .sender
            .max_capacity()
            .saturating_sub(self.sender.capacity())
        {
            self.observer.lose();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    #[derive(Debug)]
    struct Clock;
    impl grapt_types::ClockAuthority for Clock {
        fn now(&self) -> grapt_types::Timestamp {
            grapt_types::Timestamp::from_millis(0)
        }
    }
    #[tokio::test]
    async fn diagnostic_bound_preserves_utf8_and_includes_the_newline(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (writer, mut reader) = tokio::io::duplex(MAXIMUM + 16);
        let output = DiagnosticOutput::start(writer, AcpTiming::bounded(Arc::new(Clock)));
        output.send("a".repeat(MAXIMUM - 2) + "é");
        let mut bytes = vec![0; MAXIMUM - 1];
        tokio::time::timeout(Duration::from_millis(250), reader.read_exact(&mut bytes)).await??;
        assert!(
            std::str::from_utf8(&bytes).is_ok(),
            "diagnostic truncation split UTF-8"
        );
        assert_eq!(bytes.last(), Some(&b'\n'));
        drop(output);
        Ok(())
    }
}
