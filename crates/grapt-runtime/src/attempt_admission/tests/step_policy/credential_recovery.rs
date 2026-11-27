use super::*;

async fn scenario(visible: bool, attempts: u32) -> (crate::StepReport, usize) {
    let mut rejected = failure(ProviderFailureClass::Authentication);
    let AttemptFragment::ProviderFailure { classification, .. } = &mut rejected else {
        panic!("failure fixture did not return a provider failure");
    };
    *classification = Some(grapt_provider::FailureClassification::CredentialRefreshed);
    let mut first = if visible {
        text("already visible")
    } else {
        Vec::new()
    };
    first.push(rejected);
    let mut second = text("recovered answer");
    second.push(stop());
    let mut source = ScriptSource::new(vec![first, second]);
    let mut history = FakeHistory::new();
    let (_interrupt, cancel) = step_interrupt();
    let (_owner, report) = StepDriver::run(
        step(),
        policy(attempts),
        &mut source,
        &mut history,
        Arc::new(InstantTools),
        RecordingOwner::new(),
        cancel,
    )
    .await;
    assert!(
        history.calls.is_empty(),
        "credential renewal rebuilt history"
    );
    assert_eq!(report.history_reloads, 0);
    (report, source.opens.load(Ordering::SeqCst))
}

#[tokio::test(start_paused = true)]
async fn a_recovered_authentication_failure_opens_a_distinct_receipted_attempt() {
    let (report, opens) = scenario(false, 5).await;
    assert_eq!(opens, 2);
    assert_eq!(report.receipts.len(), 2);
    assert_ids_distinct(&report);
    assert_eq!(
        report.result,
        StepResult::Ended {
            finish: FinishReason::Stop
        }
    );
}

#[tokio::test(start_paused = true)]
async fn a_recovered_credential_never_retries_after_visible_output() {
    let (report, opens) = scenario(true, 5).await;
    assert_eq!(opens, 1);
    assert_eq!(report.receipts.len(), 1);
    assert!(matches!(report.result, StepResult::Failed { failure }
        if matches!(failure.reason(), StepTerminalReason::ProviderFailure {
            class: grapt_types::ProviderFailureClass::Authentication })));
}

#[tokio::test(start_paused = true)]
async fn a_recovered_credential_does_not_refund_the_attempt_budget() {
    let (report, opens) = scenario(false, 1).await;
    assert_eq!(opens, 1);
    assert_eq!(report.receipts.len(), 1);
    assert!(matches!(report.result, StepResult::Failed { failure }
        if matches!(failure.reason(), StepTerminalReason::PolicyLimit { .. })));
}
