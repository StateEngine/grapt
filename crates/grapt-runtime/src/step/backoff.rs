//! One cancellation-aware backoff for provider and pre-admission retries.
use super::*;

pub(super) async fn wait<S: AttemptSource>(
    source: &mut S,
    cancel: &mut watch::Receiver<Option<InterruptionSource>>,
    interrupt_available: &mut bool,
    notice: RetryNotice,
) -> Result<(), StepFailure> {
    source.retrying(Some(notice));
    // Pin once: losing the cancellation sender must not restart the delay.
    let mut backoff = std::pin::pin!(tokio::time::sleep(notice.after));
    let interrupted = loop {
        if let Some(by) = *cancel.borrow() {
            break Some(by);
        }
        tokio::select! {
            biased;
            changed = cancel.changed(), if *interrupt_available => {
                if changed.is_err() { *interrupt_available = false; }
            }
            _ = &mut backoff => break *cancel.borrow(),
        }
    };
    source.retrying(None);
    match interrupted {
        Some(by) => Err(StepFailure::new(
            StepTerminalReason::Interrupted { by },
            "interrupted during retry backoff",
        )),
        None => Ok(()),
    }
}
