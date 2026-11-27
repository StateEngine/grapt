use super::{
    assert_ids_distinct, failure, policy, step, step_interrupt, stop, test_version, text, Arc,
    AttemptFragment, AttemptTerminalReason, DurableFact, FakeHistory, FinishReason, InstantTools,
    Ordering, ProviderFailureClass, RecordingOwner, ScriptSource, StepDriver, StepOutcome,
    StepResult, StepTerminalReason, TerminalClass,
};

// ---------------------------------------------------------------------------
// Retry / visible output / receipts
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn preflight_failure_then_success() {
    let mut source = ScriptSource::new(vec![vec![failure(ProviderFailureClass::Transport)], {
        let mut ok = text("the answer");
        ok.push(stop());
        ok
    }]);
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

    assert_eq!(
        report.result,
        StepResult::Ended {
            finish: FinishReason::Stop
        }
    );
    assert_eq!(report.receipts.len(), 2);
    assert_eq!(source.opens.load(Ordering::SeqCst), 2, "one stream each");
    assert_eq!(report.history_reloads, 0, "a retry never reloads history");
    assert!(history.calls.is_empty(), "a retry never compacts");
    assert_ids_distinct(&report);

    let answers = owner
        .committed
        .iter()
        .filter(|fact| match fact {
            DurableFact::Fragment { fragment } => matches!(
                fragment.as_ref(),
                AttemptFragment::TextDelta { text, .. } if text == "the answer"
            ),
            _ => false,
        })
        .count();
    assert_eq!(answers, 1, "the answer is committed exactly once");
}

#[tokio::test(start_paused = true)]
async fn visible_output_failure_is_not_retried_and_keeps_partial_truth() {
    let mut source = ScriptSource::new(vec![{
        let mut partial = text("partial answer");
        partial.push(failure(ProviderFailureClass::RateLimit {
            retry_after: None,
        }));
        partial
    }]);
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

    assert_eq!(report.receipts.len(), 1, "no retry after visible output");
    assert_eq!(source.opens.load(Ordering::SeqCst), 1);
    match &report.result {
        StepResult::Failed { failure } => {
            // The provider's OWN class survives to the Step terminal. The
            // string carrier flattened every such stop to one literal.
            assert_eq!(
                failure.reason(),
                &StepTerminalReason::ProviderFailure {
                    class: TerminalClass::RateLimit
                }
            )
        }
        other => panic!("expected a failure, got {other:?}"),
    }
    assert!(
        owner.committed.iter().any(|fact| match fact {
            DurableFact::Fragment { fragment } => matches!(
                fragment.as_ref(),
                AttemptFragment::TextDelta { text, .. } if text == "partial answer"
            ),
            _ => false,
        }),
        "the partial output must survive"
    );
}

#[tokio::test(start_paused = true)]
async fn a_settled_non_retryable_provider_failure_is_carried_up_losslessly() {
    // This is deliberately a failure before visible output. Authentication is
    // non-retryable, so classify returns Settled and the Step must adopt the
    // attempt's typed reason through `carried_up`; the direct Stop path used by
    // retryable failures after output cannot satisfy this witness.
    let mut source = ScriptSource::new(vec![vec![failure(ProviderFailureClass::Authentication)]]);
    let mut history = FakeHistory::new();
    let (_interrupt, cancel) = step_interrupt();

    let (_owner, report) = StepDriver::run(
        step(),
        policy(5),
        &mut source,
        &mut history,
        Arc::new(InstantTools),
        RecordingOwner::new(),
        cancel,
    )
    .await;

    assert_eq!(
        report.receipts.len(),
        1,
        "a terminal class opens one attempt"
    );
    let receipt = &report.receipts[0];
    assert_eq!(receipt.decision, crate::AttemptDecision::Settled);
    match &receipt.outcome {
        StepOutcome::Failed { failure } => assert_eq!(
            failure.reason(),
            &AttemptTerminalReason::ProviderFailure {
                class: TerminalClass::Authentication
            },
            "the settled attempt must retain the provider class"
        ),
        other => panic!("a provider failure must fail its attempt, got {other:?}"),
    }
    match &report.result {
        StepResult::Failed { failure } => assert_eq!(
            failure.reason(),
            &StepTerminalReason::ProviderFailure {
                class: TerminalClass::Authentication
            },
            "carried_up must preserve the settled attempt's provider class"
        ),
        other => panic!("a settled provider failure must fail the Step, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn retry_stops_at_the_cap_with_one_stream_per_attempt() {
    let mut source = ScriptSource::new(vec![vec![failure(ProviderFailureClass::Transport)]]);
    let mut history = FakeHistory::new();
    let (_interrupt, cancel) = step_interrupt();
    let cap = 5u32;

    let (_owner, report) = StepDriver::run(
        step(),
        policy(cap),
        &mut source,
        &mut history,
        Arc::new(InstantTools),
        RecordingOwner::new(),
        cancel,
    )
    .await;

    assert_eq!(report.receipts.len() as u32, cap);
    assert_eq!(source.opens.load(Ordering::SeqCst) as u32, cap);
    assert_ids_distinct(&report);
    match &report.result {
        StepResult::Failed { failure } => assert_eq!(
            failure.reason(),
            &StepTerminalReason::PolicyLimit {
                policy: test_version()
            }
        ),
        other => panic!("expected cap exhaustion, got {other:?}"),
    }
}
