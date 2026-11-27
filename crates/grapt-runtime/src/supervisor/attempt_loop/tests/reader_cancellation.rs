use super::*;
use crate::supervisor::AttemptCancelCause;
use grapt_types::{AttemptTerminalReason, InterruptionSource};

async fn assert_reader_closure_preserves_cancellation(
    cause: AttemptCancelCause,
    reason: AttemptTerminalReason,
) -> Result<(), Box<dyn std::error::Error>> {
    // Model the EOF classification boundary with the cause already visible but
    // no new change notification. The real reader observes this current value
    // and closes its channel without producing a provider terminal.
    let (cancel_tx, cancel_rx) = watch::channel(Some(cause));
    let residency = DrainResidency::new();
    let (owner, report) = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        run_attempt(
            attempt("att_cancelled_reader_closure"),
            || ScriptedReader(Vec::new().into_iter()),
            Arc::new(NeverTools),
            RecordingOwner::new(),
            cancel_tx,
            cancel_rx,
            residency.token(),
            quiet_step(),
        ),
    )
    .await?;
    assert!(residency.is_closed());
    assert!(report.diagnostics.cancelled);
    assert_eq!(report.diagnostics.tools_started, 0);
    match report.outcome {
        StepOutcome::Failed { failure } => assert_eq!(failure.reason(), &reason),
        other => panic!("cancelled reader closure lost its cause: {other:?}"),
    }
    assert_eq!(owner.attempt_terminal_count(), 1);
    assert_eq!(owner.committed.len(), owner.notified.len());
    Ok(())
}

#[tokio::test]
async fn reader_closure_preserves_current_user_interrupt_without_a_new_notification(
) -> Result<(), Box<dyn std::error::Error>> {
    assert_reader_closure_preserves_cancellation(
        AttemptCancelCause::Interrupted {
            by: InterruptionSource::User,
        },
        AttemptTerminalReason::Interrupted {
            by: InterruptionSource::User,
        },
    )
    .await
}

#[tokio::test]
async fn reader_closure_preserves_current_watchdog_without_a_new_notification(
) -> Result<(), Box<dyn std::error::Error>> {
    assert_reader_closure_preserves_cancellation(
        AttemptCancelCause::NoProgress,
        AttemptTerminalReason::ProviderFailure {
            class: grapt_types::ProviderFailureClass::NoProgressTimeout,
        },
    )
    .await
}
