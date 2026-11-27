use super::{
    assert_ids_distinct, failure, overflow, policy, step, step_interrupt, stop, text, Arc,
    ContextFailure, DurableFact, FakeHistory, FinishReason, InstantTools, Ordering,
    ProviderFailureClass, RecordingOwner, RejectingOwner, ScriptSource, StepDriver, StepResult,
    StepTerminalReason,
};
use grapt_provider::FailureClassification;

/// The Step rebuilds on a classified overflow whatever transport class carries
/// it, which is what an OpenAI-compatible server's prose refusal produces.
#[tokio::test(start_paused = true)]
async fn a_compat_prose_overflow_rebuilds_before_the_next_attempt() {
    let mut source = ScriptSource::new(vec![vec![failure_with_overflow()], {
        let mut ok = text("after compaction");
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

    assert_eq!(
        history.calls,
        vec!["compact", "reload"],
        "an overflowing compat body must compact and reload, not fail the Step"
    );
    assert_eq!(report.compactions, 1);
    assert_eq!(report.history_reloads, 1);
    assert!(
        matches!(report.result, StepResult::Ended { .. }),
        "{report:?}"
    );
}

fn failure_with_overflow() -> super::AttemptFragment {
    super::AttemptFragment::ProviderFailure {
        class: ProviderFailureClass::InvalidRequest,
        classification: Some(FailureClassification::ContextOverflow),
        diagnostic: None,
        message: "the request exceeds the available context size".into(),
        state: grapt_provider::fragment::AdapterState::none(),
    }
}

#[tokio::test(start_paused = true)]
async fn a_refused_attempt_start_opens_zero_provider_streams() {
    // The R2-f ordering law, now at Step level: `open` may mint an identity and
    // prepare a factory, but the STREAM must not open until AttemptStarted has
    // committed. A refused start therefore opens nothing at all.
    let mut source = ScriptSource::new(vec![vec![stop()]]);
    let mut history = FakeHistory::new();
    let (_interrupt, cancel) = step_interrupt();

    let (owner, report) = StepDriver::run(
        step(),
        policy(5),
        &mut source,
        &mut history,
        Arc::new(InstantTools),
        RejectingOwner::rejecting(|fact| matches!(fact, DurableFact::AttemptStarted { .. })),
        cancel,
    )
    .await;

    assert_eq!(
        source.opens.load(Ordering::SeqCst),
        0,
        "a refused attempt start must open NO provider stream"
    );
    assert_eq!(
        source.mints.load(Ordering::SeqCst),
        1,
        "one identity was minted, which is allowed"
    );
    assert!(matches!(report.result, StepResult::OwnerRejected { .. }));
    assert_eq!(
        owner.inner.step_terminal_count(),
        0,
        "no Step terminal on top of an unwritten attempt"
    );
}

// ---------------------------------------------------------------------------
// Compaction is a real operation
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn compaction_runs_before_reload_and_the_rebuilt_attempt_sees_a_new_generation() {
    // "Rebuild" has to mean more than "try again": compaction happens, THEN the
    // reload yields a new generation, and the rebuilt attempt is built from it.
    let mut source = ScriptSource::new(vec![vec![overflow()], {
        let mut ok = text("after compaction");
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

    assert_eq!(
        history.calls,
        vec!["compact", "reload"],
        "compaction must run before the reload, exactly once each"
    );
    assert_eq!(report.compactions, 1);
    assert_eq!(report.history_reloads, 1);

    let generations = source.generations();
    assert_eq!(generations.len(), 2);
    assert_ne!(
        generations[0], generations[1],
        "the rebuilt attempt must observe a NEW history generation: {generations:?}"
    );
    assert_eq!(generations[1], generations[0] + 1);
    assert_eq!(
        report.receipts[1].generation.value(),
        generations[1],
        "the receipt records the generation the attempt was built from"
    );
    assert_eq!(
        report.result,
        StepResult::Ended {
            finish: FinishReason::Stop
        }
    );
}

#[tokio::test(start_paused = true)]
async fn a_failed_compaction_opens_no_further_stream() {
    let mut source = ScriptSource::new(vec![vec![overflow()]]);
    let mut history = FakeHistory::failing_compact();
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

    assert_eq!(history.calls, vec!["compact"], "reload is never reached");
    assert_eq!(
        source.opens.load(Ordering::SeqCst),
        1,
        "only the first attempt opened a stream"
    );
    match &report.result {
        StepResult::Failed { failure } => assert_eq!(
            failure.reason(),
            &StepTerminalReason::ContextFailure {
                failure: ContextFailure::CompactionFailed
            }
        ),
        other => panic!("expected a compaction failure, got {other:?}"),
    }
    assert_eq!(report.history_reloads, 0);
}

#[tokio::test(start_paused = true)]
async fn a_failed_reload_opens_no_further_stream() {
    let mut source = ScriptSource::new(vec![vec![overflow()]]);
    let mut history = FakeHistory::failing_reload();
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

    assert_eq!(history.calls, vec!["compact", "reload"]);
    assert_eq!(
        source.opens.load(Ordering::SeqCst),
        1,
        "a failed reload must not open a rebuilt stream"
    );
    match &report.result {
        StepResult::Failed { failure } => assert_eq!(
            failure.reason(),
            &StepTerminalReason::ContextFailure {
                failure: ContextFailure::CompactionFailed
            }
        ),
        other => panic!("expected a reload failure, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn a_second_overflow_is_terminal() {
    let mut source = ScriptSource::new(vec![vec![overflow()], vec![overflow()]]);
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
    assert_eq!(report.compactions, 1, "exactly one rebuild is permitted");
    match &report.result {
        StepResult::Failed { failure } => {
            assert_eq!(
                failure.reason(),
                &StepTerminalReason::ContextFailure {
                    failure: ContextFailure::Overflow
                }
            )
        }
        other => panic!("a second overflow must be terminal, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The TOTAL attempt bound covers rebuilds as well as retries
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn an_overflow_then_retry_chain_never_exceeds_the_total_bound() {
    // The bound is on the Step's coding attempts. One rebuild plus retries must
    // still stop at max_attempts, attempt 6 must never open; the rebuild's
    // summary call has its own bound and spends none of it.
    let mut source = ScriptSource::new(vec![
        vec![overflow()],
        vec![failure(ProviderFailureClass::Transport)],
    ]);
    let mut history = FakeHistory::new();
    let (_interrupt, cancel) = step_interrupt();
    let bound = 5u32;

    let (_owner, report) = StepDriver::run(
        step(),
        policy(bound),
        &mut source,
        &mut history,
        Arc::new(InstantTools),
        RecordingOwner::new(),
        cancel,
    )
    .await;

    assert_eq!(
        report.receipts.len() as u32,
        bound,
        "the summary call spends none of the coding attempts"
    );
    assert_eq!(
        source.opens.load(Ordering::SeqCst) as u32,
        bound,
        "attempt {} must never open",
        bound + 1
    );
    assert_eq!(
        source.mints.load(Ordering::SeqCst) as u32,
        bound,
        "no identity is minted past the bound either"
    );
    assert_eq!(report.compactions, 1);
    assert_ids_distinct(&report);
}
