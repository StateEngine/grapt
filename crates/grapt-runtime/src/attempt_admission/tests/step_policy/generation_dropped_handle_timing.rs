use super::{
    failure, overflow, policy, step, step_interrupt, stop, test_version, text, Arc, ContextFailure,
    Duration, FakeHistory, FinishReason, InstantTools, Ordering, ProviderFailureClass,
    RecordingOwner, ScriptSource, StepDriver, StepPolicy, StepResult, StepTerminalReason,
};

#[tokio::test(start_paused = true)]
async fn a_dropped_interrupt_handle_still_observes_backoff() {
    // (C) `changed()` returns Err immediately once every sender is dropped.
    // Treating that as an interrupt would skip the backoff; ignoring the Result
    // would spin. The delay must still be waited.
    let mut source = ScriptSource::new(vec![vec![failure(ProviderFailureClass::Transport)], {
        let mut ok = text("after backoff");
        ok.push(stop());
        ok
    }]);
    let mut history = FakeHistory::new();
    let (interrupt, cancel) = step_interrupt();
    drop(interrupt); // no interrupt can ever arrive

    let started = tokio::time::Instant::now();
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
    let elapsed = started.elapsed();

    assert_eq!(
        report.result,
        StepResult::Ended {
            finish: FinishReason::Stop
        },
        "the Step must still complete normally without an interrupt handle"
    );
    assert_eq!(report.receipts.len(), 2);
    assert!(
        elapsed >= Duration::from_secs(2),
        "the 2s backoff must still be observed, waited only {elapsed:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_dropped_interrupt_handle_still_times_out_a_silent_stream_once() {
    // Same finding, watchdog side: the stall window must still fire exactly
    // once rather than the loop spinning on a closed channel.
    let mut source = ScriptSource::stalling(vec![Vec::new()]);
    let mut history = FakeHistory::new();
    let (interrupt, cancel) = step_interrupt();
    drop(interrupt);
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
        StepResult::Failed { failure } => {
            assert_eq!(
                failure.reason(),
                &StepTerminalReason::PolicyLimit {
                    policy: test_version()
                }
            )
        }
        other => panic!("expected a timeout, got {other:?}"),
    }
    assert!(elapsed >= window, "waited {elapsed:?}");
    assert!(
        elapsed < window * 2,
        "waited {elapsed:?}, more than one window"
    );
}

#[tokio::test(start_paused = true)]
async fn a_reload_that_does_not_advance_the_generation_opens_no_new_stream() {
    // (D) The rebuilt attempt must observe NEWER history. A generation that
    // stood still means the reload did not deliver what compaction promised.
    let mut source = ScriptSource::new(vec![vec![overflow()]]);
    // Reload returns the same generation the attempt already used.
    let mut history = FakeHistory::reload_returning(1);
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
        "an unchanged generation must not produce a rebuilt stream"
    );
    assert_eq!(
        report.history_reloads, 0,
        "the reload is not counted as good"
    );
    match &report.result {
        StepResult::Failed { failure } => {
            assert_eq!(
                failure.reason(),
                &StepTerminalReason::ContextFailure {
                    failure: ContextFailure::CompactionNoProgress
                }
            );
            assert!(failure.detail().as_str().contains("does not advance"));
        }
        other => panic!("expected a generation failure, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn a_regressed_generation_is_refused_too() {
    let mut source = ScriptSource::new(vec![vec![overflow()]]);
    // Older than the one in force.
    let mut history = FakeHistory::reload_returning(0);
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

    assert_eq!(source.opens.load(Ordering::SeqCst), 1);
    match &report.result {
        StepResult::Failed { failure } => {
            assert_eq!(
                failure.reason(),
                &StepTerminalReason::ContextFailure {
                    failure: ContextFailure::CompactionNoProgress
                }
            )
        }
        other => panic!("a regressed generation must be refused, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Channel closure MID-phase must not restart the phase
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn dropping_the_handle_midway_through_backoff_does_not_extend_it() {
    // Closing the interrupt channel removes only the cancel branch. If the
    // backoff sleep were recreated on closure, the elapsed portion would be
    // discarded and the delay would stretch toward 2x.
    let mut source = ScriptSource::new(vec![vec![failure(ProviderFailureClass::Transport)], {
        let mut ok = text("after backoff");
        ok.push(stop());
        ok
    }]);
    let mut history = FakeHistory::new();
    let (interrupt, cancel) = step_interrupt();
    let backoff = Duration::from_secs(2);

    let driver = StepDriver::run(
        step(),
        policy(5),
        &mut source,
        &mut history,
        Arc::new(InstantTools),
        RecordingOwner::new(),
        cancel,
    );
    let dropper = async {
        // Halfway into the 2s backoff.
        tokio::time::sleep(Duration::from_secs(1)).await;
        drop(interrupt);
    };

    let started = tokio::time::Instant::now();
    let ((_owner, report), ()) = tokio::join!(driver, dropper);
    let elapsed = started.elapsed();

    assert_eq!(
        report.result,
        StepResult::Ended {
            finish: FinishReason::Stop
        }
    );
    assert_eq!(report.receipts.len(), 2);
    // Tight on purpose. Restarting the sleep on closure produced 1s + 2s = 3s;
    // a `< 2 * backoff` bound would have passed that. Only the preserved sleep
    // lands at the original 2s.
    assert!(
        elapsed >= backoff,
        "the full backoff must still elapse; took {elapsed:?}"
    );
    assert!(
        elapsed < backoff + Duration::from_millis(100),
        "closing the channel mid-backoff must not restart it; took {elapsed:?},          expected about {backoff:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn dropping_the_handle_just_before_the_watchdog_deadline_does_not_extend_it() {
    // Same law for the stall window: a closing channel must not hand the stream
    // a fresh window.
    let mut source = ScriptSource::stalling(vec![Vec::new()]);
    let mut history = FakeHistory::new();
    let (interrupt, cancel) = step_interrupt();
    let window = Duration::from_secs(30);
    let policy = match StepPolicy::new(test_version(), 1, Duration::from_secs(2), 1, window) {
        Ok(policy) => policy,
        Err(error) => panic!("valid policy: {error}"),
    };

    let driver = StepDriver::run(
        step(),
        policy,
        &mut source,
        &mut history,
        Arc::new(InstantTools),
        RecordingOwner::new(),
        cancel,
    );
    let dropper = async {
        // Just before the 30s deadline.
        tokio::time::sleep(Duration::from_secs(29)).await;
        drop(interrupt);
    };

    let started = tokio::time::Instant::now();
    let ((_owner, report), ()) = tokio::join!(driver, dropper);
    let elapsed = started.elapsed();

    match &report.result {
        StepResult::Failed { failure } => {
            assert_eq!(
                failure.reason(),
                &StepTerminalReason::PolicyLimit {
                    policy: test_version()
                }
            )
        }
        other => panic!("expected a timeout, got {other:?}"),
    }
    // Tight on purpose. Recreating the window on closure produced 29s + 30s =
    // 59s, which a `< 2 * window` bound would have accepted.
    assert!(
        elapsed >= window,
        "the window must still elapse; took {elapsed:?}"
    );
    assert!(
        elapsed < window + Duration::from_secs(1),
        "closing the channel near the deadline must not grant a fresh window;          took {elapsed:?}, expected about {window:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn dropping_the_handle_midway_through_compaction_does_not_repeat_the_provider_call() {
    // The compaction future is created once. Dropping it on channel closure and
    // calling compact() again would duplicate a provider call — the worst of
    // the three, because it is an external effect rather than a delay.
    let mut source = ScriptSource::new(vec![vec![overflow()], {
        let mut ok = text("after compaction");
        ok.push(stop());
        ok
    }]);
    let compaction = Duration::from_secs(10);
    let mut history = FakeHistory::slow_compact(compaction);
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
    let dropper = async {
        // Halfway through the compaction call.
        tokio::time::sleep(Duration::from_secs(5)).await;
        drop(interrupt);
    };

    let started = tokio::time::Instant::now();
    let ((_owner, report), ()) = tokio::join!(driver, dropper);
    let elapsed = started.elapsed();

    assert_eq!(
        history.compact_calls, 1,
        "the compaction provider call must not be repeated on channel closure"
    );
    assert_eq!(history.calls, vec!["compact", "reload"]);
    assert_eq!(report.compactions, 1);
    assert_eq!(report.history_reloads, 1);
    assert_eq!(
        report.result,
        StepResult::Ended {
            finish: FinishReason::Stop
        },
        "the Step still completes; closure only removes the cancel branch"
    );
    assert!(
        elapsed < compaction + Duration::from_millis(100),
        "one compaction duration, not two; took {elapsed:?}, expected about {compaction:?}"
    );
}
