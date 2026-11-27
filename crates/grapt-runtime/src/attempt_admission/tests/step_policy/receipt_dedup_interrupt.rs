use super::{
    failure, overflow, policy, step, step_interrupt, stop, text, Arc, AttemptFragment, DurableFact,
    Duration, FakeHistory, FinishReason, InstantTools, InterruptionSource, Ordering,
    ProviderFailureClass, RecordingOwner, ScriptSource, StepDriver, StepResult, StepTerminalReason,
};

#[tokio::test(start_paused = true)]
async fn a_retried_step_commits_exactly_one_step_terminal() {
    // Each ATTEMPT writes its own terminal; the logical Step gets exactly one.
    let mut source = ScriptSource::new(vec![vec![failure(ProviderFailureClass::Transport)]]);
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

    assert_eq!(report.receipts.len(), 5);
    assert_eq!(owner.attempt_terminal_count(), 5, "one per attempt");
    assert_eq!(owner.step_terminal_count(), 1, "exactly one for the Step");
    match owner.committed.last() {
        Some(DurableFact::StepFailed { .. }) => {}
        other => panic!("the Step terminal must be last, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn retries_and_rebuilds_never_duplicate_model_visible_content() {
    let mut source = ScriptSource::new(vec![
        vec![overflow()],
        vec![failure(ProviderFailureClass::Transport)],
        vec![failure(ProviderFailureClass::Transport)],
        {
            let mut ok = text("the one answer");
            ok.push(stop());
            ok
        },
    ]);
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
    assert_eq!(report.receipts.len(), 4);
    // Reloads track compactions, not attempts. A retry that reloaded would
    // make this 3.
    assert_eq!(report.compactions, 1);
    assert_eq!(report.history_reloads, 1);
    assert_eq!(history.calls, vec!["compact", "reload"]);

    let answers = owner
        .committed
        .iter()
        .filter(|fact| match fact {
            DurableFact::Fragment { fragment } => matches!(
                fragment.as_ref(),
                AttemptFragment::TextDelta { text, .. } if text == "the one answer"
            ),
            _ => false,
        })
        .count();
    assert_eq!(answers, 1, "the answer must not be duplicated");
    assert_eq!(owner.attempt_terminal_count(), 4);
    assert_eq!(owner.step_terminal_count(), 1);
}

#[tokio::test(start_paused = true)]
async fn receipts_record_identity_ordinal_generation_and_decision() {
    let mut source = ScriptSource::new(vec![vec![failure(ProviderFailureClass::Transport)], {
        let mut ok = text("done");
        ok.push(stop());
        ok
    }]);
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

    assert_eq!(report.receipts.len(), 2);
    assert!(matches!(
        report.receipts[0].decision,
        crate::AttemptDecision::Retry { .. }
    ));
    assert_eq!(report.receipts[1].decision, crate::AttemptDecision::Settled);
    assert_eq!(report.receipts[0].ordinal, 1);
    assert_eq!(report.receipts[1].ordinal, 2);
    for receipt in &report.receipts {
        assert_eq!(receipt.step, step());
        assert_eq!(
            receipt.generation.value(),
            1,
            "no compaction happened, so the generation never moved"
        );
    }
}

// ---------------------------------------------------------------------------
// Round-2 findings: compaction is a provider call; dropped handles; generations
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn an_interrupt_during_compaction_stops_before_reload_or_a_new_attempt() {
    // (A) Compaction is its own provider call, so it is awaited — and it must
    // be interruptible while it runs. An interrupt there must stop before the
    // reload and before any rebuilt stream.
    let mut source = ScriptSource::new(vec![vec![overflow()]]);
    let mut history = FakeHistory::slow_compact(Duration::from_secs(10));
    let (interrupt, cancel) = step_interrupt();

    let driver = StepDriver::run(
        step(),
        policy(5),
        &mut source,
        &mut history,
        Arc::new(InstantTools),
        RecordingOwner::new(),
        cancel,
    );
    let interrupter = async {
        // Land inside the compaction call.
        tokio::time::sleep(Duration::from_secs(1)).await;
        interrupt.interrupt();
    };
    let ((_owner, report), ()) = tokio::join!(driver, interrupter);

    assert_eq!(
        history.compact_calls, 1,
        "exactly one compaction provider call"
    );
    assert_eq!(
        history.calls,
        vec!["compact"],
        "the reload must never be reached"
    );
    assert_eq!(report.history_reloads, 0);
    assert_eq!(
        source.opens.load(Ordering::SeqCst),
        1,
        "no rebuilt stream may open after an interrupt during compaction"
    );
    match &report.result {
        StepResult::Failed { failure } => {
            assert_eq!(
                failure.reason(),
                &StepTerminalReason::Interrupted {
                    by: InterruptionSource::User
                }
            );
            assert!(failure.detail().as_str().contains("compaction"));
        }
        other => panic!("expected aborted during compaction, got {other:?}"),
    }
}
