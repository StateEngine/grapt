use super::{
    failure, policy, step, step_interrupt, stop, test_version, text, Arc, DurableFact, Duration,
    FakeHistory, InstantTools, InterruptionSource, Ordering, ProviderFailureClass, RecordingOwner,
    ScriptSource, StepDriver, StepPolicy, StepResult, StepTerminalReason, TerminalClass,
};

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_pre_cancelled_step_opens_zero_streams_and_mints_nothing() {
    let mut source = ScriptSource::new(vec![vec![stop()]]);
    let mut history = FakeHistory::new();
    let (interrupt, cancel) = step_interrupt();
    interrupt.interrupt();

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
        source.opens.load(Ordering::SeqCst),
        0,
        "a pre-cancelled Step must open no stream"
    );
    assert_eq!(
        source.mints.load(Ordering::SeqCst),
        0,
        "cancellation is checked before minting an identity"
    );
    assert!(report.receipts.is_empty(), "no attempt ran");
    match &report.result {
        StepResult::Failed { failure } => assert_eq!(
            failure.reason(),
            &StepTerminalReason::Interrupted {
                by: InterruptionSource::User
            }
        ),
        other => panic!("expected aborted, got {other:?}"),
    }
    assert_eq!(
        owner.step_terminal_count(),
        1,
        "the Step still records its one terminal"
    );
}

#[tokio::test(start_paused = true)]
async fn an_interrupt_during_retry_backoff_completes_without_another_attempt() {
    // The backoff is 2s. Interrupting during it must abandon the wait rather
    // than sleeping it out and then opening another stream.
    let mut source = ScriptSource::new(vec![vec![failure(ProviderFailureClass::Transport)]]);
    let mut history = FakeHistory::new();
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
        tokio::time::sleep(Duration::from_millis(100)).await;
        interrupt.interrupt();
    };
    let ((owner, report), ()) = tokio::join!(driver, interrupter);

    assert_eq!(
        report.receipts.len(),
        1,
        "the interrupt must not buy a second attempt"
    );
    assert_eq!(source.opens.load(Ordering::SeqCst), 1, "one stream only");
    match &report.result {
        StepResult::Failed { failure } => {
            assert_eq!(
                failure.reason(),
                &StepTerminalReason::Interrupted {
                    by: InterruptionSource::User
                }
            );
            assert!(failure.detail().as_str().contains("backoff"));
        }
        other => panic!("expected aborted during backoff, got {other:?}"),
    }
    assert_eq!(
        owner
            .committed
            .iter()
            .filter(|fact| matches!(fact, DurableFact::AttemptStarted { .. }))
            .count(),
        1,
        "no second AttemptStarted"
    );
}

// ---------------------------------------------------------------------------
// No-progress timeout: one window after the last commit
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_silent_stream_times_out_after_exactly_one_window() {
    // The window measures silence since the last COMMIT. AttemptStarted commits
    // immediately, so a silent stream must fail one window later — not two,
    // which is what sampling a counter before the start fact produced.
    let mut source = ScriptSource::stalling(vec![Vec::new()]);
    let mut history = FakeHistory::new();
    let (_interrupt, cancel) = step_interrupt();
    let window = Duration::from_secs(30);
    let policy = match StepPolicy::new(test_version(), 1, Duration::from_secs(2), 1, window) {
        Ok(policy) => policy,
        Err(error) => panic!("valid policy: {error}"),
    };

    let started = tokio::time::Instant::now();
    let (_owner, report) = StepDriver::run(
        step(),
        policy,
        &mut source,
        &mut history,
        Arc::new(InstantTools),
        RecordingOwner::new(),
        cancel,
    )
    .await;
    let elapsed = started.elapsed();

    match &report.result {
        StepResult::Failed { failure } => assert_eq!(
            failure.reason(),
            &StepTerminalReason::PolicyLimit {
                policy: test_version()
            },
            "a silent stream is a typed timeout"
        ),
        other => panic!("expected a timeout, got {other:?}"),
    }
    assert!(
        elapsed >= window,
        "must wait the full window before declaring a stall, waited {elapsed:?}"
    );
    assert!(
        elapsed < window * 2,
        "timed out after {elapsed:?}, which is more than a single {window:?} window"
    );
}

#[tokio::test(start_paused = true)]
async fn a_mid_step_stall_times_out_one_window_after_the_last_commit() {
    // Progress happens, then silence. The deadline resets on that progress, so
    // the total stays inside two windows rather than accumulating a stale one.
    let mut source = ScriptSource::stalling(vec![text("started well")]);
    let mut history = FakeHistory::new();
    let (_interrupt, cancel) = step_interrupt();
    let window = Duration::from_secs(30);
    let policy = match StepPolicy::new(test_version(), 5, Duration::from_secs(2), 1, window) {
        Ok(policy) => policy,
        Err(error) => panic!("valid policy: {error}"),
    };

    let started = tokio::time::Instant::now();
    let (_owner, report) = StepDriver::run(
        step(),
        policy,
        &mut source,
        &mut history,
        Arc::new(InstantTools),
        RecordingOwner::new(),
        cancel,
    )
    .await;
    let elapsed = started.elapsed();

    assert_eq!(report.receipts.len(), 1, "no retry after visible output");
    match &report.result {
        StepResult::Failed { failure } => {
            assert_eq!(
                failure.reason(),
                &StepTerminalReason::ProviderFailure {
                    class: TerminalClass::NoProgressTimeout
                }
            )
        }
        other => panic!("expected a timeout, got {other:?}"),
    }
    assert!(
        elapsed < window * 2,
        "a reset deadline must not allow nearly two windows of stall: {elapsed:?}"
    );
}
