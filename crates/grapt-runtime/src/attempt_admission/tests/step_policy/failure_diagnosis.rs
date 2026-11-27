//! The driver keeps an optional diagnosis without changing its terminal reason.
use super::*;
use grapt_types::{
    failure_diagnostic::{RetryFailureClass, StepFailureDiagnostic, UsageDiagnostic, UsageIssue},
    usage::UsageFieldKind,
};

#[tokio::test(start_paused = true)]
async fn usage_diagnosis_survives_attempt_to_step_and_the_owner_callback() {
    let diagnostic = UsageDiagnostic {
        field: Some(UsageFieldKind::Output),
        issue: UsageIssue::ConflictingSnapshots,
    };
    let mut fragment = failure(ProviderFailureClass::InvalidProviderOutput);
    let AttemptFragment::ProviderFailure {
        diagnostic: carried,
        ..
    } = &mut fragment
    else {
        panic!("fixture shape changed")
    };
    *carried = Some(diagnostic);
    let mut source = ScriptSource::new(vec![vec![fragment]]);
    let mut history = FakeHistory::new();
    let (_interrupt, cancel) = step_interrupt();
    let (owner, report) = StepDriver::run(
        step(),
        policy(5),
        &mut source,
        &mut history,
        Arc::new(InstantTools),
        RecordingOwner::new(),
        cancel,
    )
    .await;
    let StepResult::Failed { failure } = report.result else {
        panic!("usage must fail")
    };
    assert_eq!(
        failure.diagnostic(),
        Some(StepFailureDiagnostic::Usage(diagnostic))
    );
    assert_eq!(source.opens.load(Ordering::SeqCst), 1);
    assert!(owner.notified.iter().any(|fact| matches!(fact, DurableFact::StepFailed {failure} if failure.diagnostic()==Some(StepFailureDiagnostic::Usage(diagnostic)))), "the settled callback lost the diagnosis");
}
#[tokio::test(start_paused = true)]
async fn retry_exhaustion_uses_this_steps_receipts_and_last_class() {
    let scripts = vec![
        vec![failure(ProviderFailureClass::RateLimit {
            retry_after: None,
        })],
        vec![failure(ProviderFailureClass::RateLimit {
            retry_after: None,
        })],
        vec![failure(ProviderFailureClass::ProviderInternal {
            retry_after: None,
        })],
        vec![failure(ProviderFailureClass::ProviderInternal {
            retry_after: None,
        })],
        vec![failure(ProviderFailureClass::ProviderInternal {
            retry_after: None,
        })],
    ];
    let mut source = ScriptSource::new(scripts);
    let mut history = FakeHistory::new();
    let (_interrupt, cancel) = step_interrupt();
    let (_, report) = StepDriver::run(
        step(),
        policy(5),
        &mut source,
        &mut history,
        Arc::new(InstantTools),
        RecordingOwner::new(),
        cancel,
    )
    .await;
    let StepResult::Failed { failure } = report.result else {
        panic!("retry cap must fail")
    };
    let Some(StepFailureDiagnostic::RetryExhausted(retry)) = failure.diagnostic() else {
        panic!("retry diagnosis missing")
    };
    assert_eq!(retry.attempts(), 5);
    use RetryFailureClass::{RateLimited, ServerError};
    assert_eq!(
        retry.classes(),
        &[
            RateLimited,
            RateLimited,
            ServerError,
            ServerError,
            ServerError
        ]
    );
    assert_eq!(retry.last(), ServerError);
    assert_eq!(report.receipts.len(), 5);
    assert_eq!(source.opens.load(Ordering::SeqCst), 5);
    assert!(matches!(
        failure.reason(),
        StepTerminalReason::PolicyLimit { .. }
    ));
}
#[tokio::test(start_paused = true)]
async fn a_nonretryable_provider_failure_does_not_invent_a_retry_diagnosis() {
    let mut source = ScriptSource::new(vec![vec![failure(ProviderFailureClass::Authentication)]]);
    let mut history = FakeHistory::new();
    let (_interrupt, cancel) = step_interrupt();
    let (_, report) = StepDriver::run(
        step(),
        policy(5),
        &mut source,
        &mut history,
        Arc::new(InstantTools),
        RecordingOwner::new(),
        cancel,
    )
    .await;
    let StepResult::Failed { failure } = report.result else {
        panic!("authentication must fail")
    };
    assert_eq!(failure.diagnostic(), None);
    assert_eq!(source.opens.load(Ordering::SeqCst), 1);
}
