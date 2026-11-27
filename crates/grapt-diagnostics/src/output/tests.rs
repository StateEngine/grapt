//! An actual worker survives a recoverable sink failure and reports its loss.
use super::*;
use tokio::sync::oneshot;

const LOSS: &[u8] = b"GRAPT_PROVIDER_TAIL_LOSS";

#[derive(Clone, Copy)]
enum Fault {
    Data,
    Notice,
}

struct RecoveringSink {
    fault: Fault,
    failed: bool,
    bytes: Vec<u8>,
    finished: Option<oneshot::Sender<Vec<u8>>>,
}

impl Write for RecoveringSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let notice = bytes.starts_with(LOSS);
        if !self.failed
            && matches!(
                (self.fault, notice),
                (Fault::Data, false) | (Fault::Notice, true)
            )
        {
            self.failed = true;
            return Err(io::Error::other(
                "injected temporary diagnostic sink failure",
            ));
        }
        assert!(
            self.bytes.len() + bytes.len() <= 4096,
            "diagnostic fixture exceeded its byte bound"
        );
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for RecoveringSink {
    fn drop(&mut self) {
        if let Some(finished) = self.finished.take() {
            let _receiver_gone = finished.send(std::mem::take(&mut self.bytes));
        }
    }
}

async fn recovers(fault: Fault) -> Result<(), Box<dyn std::error::Error>> {
    let (finished, result) = oneshot::channel();
    let initial = if matches!(fault, Fault::Notice) { 3 } else { 0 };
    let lost = Arc::new(AtomicU64::new(initial));
    let output = LossyOutput::start(
        RecoveringSink {
            fault,
            failed: false,
            bytes: Vec::new(),
            finished: Some(finished),
        },
        Arc::clone(&lost),
        "GRAPT_PROVIDER_TAIL_LOSS tail_count_at_least",
    )?;
    output.send("first-line\n".into());
    output.send("after-loss\n".into());
    output.send("still-writing\n".into());
    drop(output);
    // Completion is the real sink's drop after the worker drains its queue.
    let bytes = tokio::time::timeout(std::time::Duration::from_secs(3), result).await??;
    let text = std::str::from_utf8(&bytes)?;
    assert!(
        text.contains("after-loss\n") && text.contains("still-writing\n"),
        "temporary output failure permanently silenced the log: {text}"
    );
    assert_eq!(lost.load(Ordering::Relaxed), initial + 1);
    assert!(
        text.contains(&format!(
            "GRAPT_PROVIDER_TAIL_LOSS tail_count_at_least={}\n",
            initial + 1
        )),
        "the dropped line or loss notice went unreported: {text}"
    );
    Ok(())
}

#[tokio::test]
async fn transient_data_write_failure_keeps_later_log_lines_and_loss_count(
) -> Result<(), Box<dyn std::error::Error>> {
    recovers(Fault::Data).await
}

#[tokio::test]
async fn transient_loss_notice_failure_retries_the_notice_and_keeps_writing(
) -> Result<(), Box<dyn std::error::Error>> {
    recovers(Fault::Notice).await
}
