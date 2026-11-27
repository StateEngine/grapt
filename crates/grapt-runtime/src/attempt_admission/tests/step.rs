use super::{
    attempt, prepare_admitted, residency, stop, text_block, tool_called, Arc, AtomicUsize,
    AttemptCancelCause, AttemptTerminalReason, BarrierTools, BoxFuture, DurableFact, Duration,
    InstantTools, InterruptionSource, Notify, Ordering, RecordingOwner, RejectingOwner,
    ScriptedReader, StepOutcome, ToolCallId, ToolOutcome, ToolRunner,
};
use grapt_provider::{AttemptFragment, FinishReason};

// Witness 5 — one stream per PhysicalAttempt
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exactly_one_provider_stream_is_opened_per_physical_attempt() {
    // Contract rule 1, oracle anchor llm.ts:355. `prepare` takes a ONE-SHOT
    // FACTORY, so the stream is opened by the supervisor rather than handed in
    // already-open. Counting factory invocations is the real proof; counting
    // next() polls would only show how often one stream was read.
    let opens = Arc::new(AtomicUsize::new(0));
    let opens_in_factory = Arc::clone(&opens);

    let mut script = text_block(0, "one");
    script.push(stop());

    let (_interrupt, run) = prepare_admitted(
        attempt("att_single"),
        move || {
            opens_in_factory.fetch_add(1, Ordering::SeqCst);
            ScriptedReader::new(script)
        },
        Arc::new(InstantTools),
        RecordingOwner::new(),
        residency(),
    );
    let (owner, report) = run.await;

    assert_eq!(
        report.outcome,
        StepOutcome::Ended {
            finish: FinishReason::Stop
        }
    );
    assert_eq!(
        opens.load(Ordering::SeqCst),
        1,
        "the provider stream must be opened exactly once"
    );
    assert_eq!(
        owner
            .committed
            .iter()
            .filter(|fact| matches!(fact, DurableFact::AttemptStarted { .. }))
            .count(),
        1,
        "exactly one attempt start"
    );
    assert_eq!(owner.attempt_terminal_count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_attempt_start_opens_no_stream_at_all() {
    // The ordering law: no provider I/O may happen until the attempt-start
    // fact is durable. If the owner refuses it, the factory must never run.
    let opens = Arc::new(AtomicUsize::new(0));
    let opens_in_factory = Arc::clone(&opens);

    let (_interrupt, run) = prepare_admitted(
        attempt("att_refused_start"),
        move || {
            opens_in_factory.fetch_add(1, Ordering::SeqCst);
            ScriptedReader::new(vec![stop()])
        },
        Arc::new(InstantTools),
        RejectingOwner::rejecting_first_matching(|fact| {
            matches!(fact, DurableFact::AttemptStarted { .. })
        }),
        residency(),
    );
    let (owner, report) = run.await;

    assert_eq!(
        opens.load(Ordering::SeqCst),
        0,
        "a refused attempt start must not open a provider stream"
    );
    match report.outcome {
        StepOutcome::OwnerRejected { error } => assert!(error.reason.contains("refused")),
        other => panic!("expected OwnerRejected, got {other:?}"),
    }
    assert_eq!(
        owner.inner.attempt_terminal_count(),
        0,
        "no step terminal may be written on top of an unwritten attempt"
    );
    assert!(
        owner.inner.notified.is_empty(),
        "notify must never run for a refused commit"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_tool_call_never_launches_the_tool() {
    // Contract step 5: a local tool starts only AFTER its ToolCalled fact is
    // committed. A refusal must therefore stop before the tool runs.
    let launched = Arc::new(AtomicUsize::new(0));

    struct CountingTools {
        launched: Arc<AtomicUsize>,
    }
    impl ToolRunner for CountingTools {
        fn run(
            &self,
            _call: ToolCallId,
            _tool: String,
            _input: crate::tool_protocol::BoundedToolInput,
        ) -> BoxFuture<'static, ToolOutcome> {
            self.launched.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { ToolOutcome::Completed { output: None } })
        }
    }

    let script = vec![tool_called("call-x"), stop()];
    let launched_probe = Arc::clone(&launched);
    let (_interrupt, run) = prepare_admitted(
        attempt("att_refused_tool"),
        move || ScriptedReader::new(script),
        Arc::new(CountingTools { launched }),
        RejectingOwner::rejecting_first_matching(|fact| match fact {
            DurableFact::Fragment { fragment } => {
                matches!(fragment.as_ref(), AttemptFragment::ToolCalled { .. })
            }
            _ => false,
        }),
        residency(),
    );
    let (owner, report) = run.await;

    assert_eq!(
        launched_probe.load(Ordering::SeqCst),
        0,
        "a refused ToolCalled must not launch the tool"
    );
    assert!(matches!(report.outcome, StepOutcome::OwnerRejected { .. }));
    assert_eq!(
        owner.inner.attempt_terminal_count(),
        0,
        "no step terminal after an owner refusal"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_step_terminal_never_reports_success() {
    // The worst case: everything streamed cleanly but the store would not
    // accept the terminal. Reporting Ended there would claim a durable success
    // that does not exist.
    let mut script = text_block(0, "all fine");
    script.push(stop());

    let (_interrupt, run) = prepare_admitted(
        attempt("att_refused_terminal"),
        move || ScriptedReader::new(script),
        Arc::new(InstantTools),
        RejectingOwner::rejecting_first_matching(|fact| {
            matches!(
                fact,
                DurableFact::AttemptEnded { .. } | DurableFact::AttemptFailed { .. }
            )
        }),
        residency(),
    );
    let (_owner, report) = run.await;

    match report.outcome {
        StepOutcome::OwnerRejected { error } => assert!(error.reason.contains("refused")),
        other => panic!("a refused terminal must not report success, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Witness 7 - an interrupt during clean settlement is still observed
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_after_a_clean_provider_terminal_still_settles_as_aborted() {
    // The gap: after the provider terminal the loop exits, and settlement used
    // to stop watching for cancellation. An interrupt arriving then would abort
    // the tools and STILL report StepEnded - a success claim over aborted work.
    let release = Arc::new(Notify::new());
    let inside = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    // A blocked tool, then a CLEAN provider terminal. The attempt is now in
    // settlement, joining that tool, when the interrupt lands.
    let script = vec![tool_called("call-late"), stop()];

    let tools = Arc::new(BarrierTools {
        release,
        inside: Arc::clone(&inside),
        peak,
    });
    let (interrupt, run) = prepare_admitted(
        attempt("att_late_interrupt"),
        move || ScriptedReader::new(script),
        tools,
        RecordingOwner::new(),
        residency(),
    );
    let handle = tokio::spawn(run);

    for _ in 0..200 {
        if inside.load(Ordering::SeqCst) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    interrupt.interrupt(AttemptCancelCause::Interrupted {
        by: InterruptionSource::User,
    });

    let (owner, report) = match tokio::time::timeout(Duration::from_secs(5), handle).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => panic!("attempt task panicked: {error}"),
        Err(_) => panic!("interrupt during settlement did not complete"),
    };

    assert!(
        report.diagnostics.cancelled,
        "an interrupt during settlement must be observed; diagnostics: {:?}",
        report.diagnostics
    );
    match report.outcome {
        StepOutcome::Failed { failure } => assert_eq!(
            failure.reason(),
            &AttemptTerminalReason::Interrupted {
                by: InterruptionSource::User
            },
            "a clean provider terminal must not survive an interrupt as success"
        ),
        other => panic!("expected an aborted failure, got {other:?}"),
    }
    for (call, outcome) in owner.tool_terminals() {
        assert!(
            !outcome.is_success(),
            "{call:?} settled as success despite the interrupt"
        );
    }
    assert_eq!(owner.attempt_terminal_count(), 1);
}

// ---------------------------------------------------------------------------
