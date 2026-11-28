use super::{
    attempt, prepare_admitted, residency, signalled, stop, text_block, tool_called, Arc,
    AtomicUsize, AttemptCancelCause, AttemptTerminalReason, BarrierTools, CommitGate,
    DrainResidency, DurableFact, Duration, HeldOwner, HungReader, InstantTools, InterruptionSource,
    Notify, Observation, RecordingOwner, Saturation, ScriptedReader, StepOutcome,
    PROVIDER_CHANNEL_CAPACITY,
};

// Witness 3 — saturated provider + interrupt cannot deadlock
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saturated_provider_plus_interrupt_settles_every_tool_before_step_failed() {
    // Topology finding: tool terminals must drain even while the
    // provider channel is saturated, "or interrupt-settlement deadlocks".
    let release = Arc::new(Notify::new());
    let inside = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    // Two blocking tools, then far more fragments than the channel holds, so
    // the reader is parked on a full channel when the interrupt lands. Deltas
    // are left out: the reader holds a delta back to coalesce it, and every
    // other fragment is sent as soon as it is read.
    let mut script = vec![tool_called("call-a"), tool_called("call-b")];
    for ordinal in 0..(PROVIDER_CHANNEL_CAPACITY as u32 * 3) {
        let block = text_block(ordinal, "saturate").into_iter();
        script.extend(block.filter(|fragment| !fragment.is_coalescible_delta()));
    }
    script.push(stop());

    let tools = Arc::new(BarrierTools {
        release,
        inside: Arc::clone(&inside),
        peak,
    });
    // The owner holds the first text commit made while both tools run, so the
    // loop stops draining and the provider channel behind it fills. This is
    // the condition the deadlock would need.
    let gate = Arc::new(CommitGate::new(Duration::from_secs(10)));
    let saturation = Arc::new(Saturation::new(PROVIDER_CHANNEL_CAPACITY));
    let owner = HeldOwner::new(
        Arc::clone(&gate),
        Arc::clone(&saturation),
        Arc::clone(&inside),
    );
    let residency = DrainResidency::new();
    let reported = Arc::clone(&saturation);
    let (interrupt, run) = prepare_admitted(
        attempt("att_saturated"),
        move || ScriptedReader::new(script).reporting_to(reported),
        tools,
        owner,
        residency.token(),
    );
    let handle = tokio::spawn(run);

    // Interrupt only once both tools are running and the reader's reservation
    // is waiting on the full channel behind the held loop.
    signalled(
        &saturation.parked,
        "no reservation waited on a full channel",
    )
    .await;
    interrupt.interrupt(AttemptCancelCause::Interrupted {
        by: InterruptionSource::User,
    });
    gate.release();

    // The whole thing must finish promptly. If it deadlocks, this times out.
    let (owner, report) = match tokio::time::timeout(Duration::from_secs(5), handle).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => panic!("attempt task panicked: {error}"),
        Err(_) => panic!("DEADLOCK: interrupt did not settle within 5s"),
    };
    let owner = owner.inner;

    assert!(
        residency.is_closed(),
        "the attempt must join every owned task before it returns"
    );

    assert!(
        report.diagnostics.cancelled,
        "the interrupt must be observed; diagnostics: {:?}",
        report.diagnostics
    );
    assert_eq!(
        report.diagnostics.provider_high_water, PROVIDER_CHANNEL_CAPACITY,
        "the provider channel filled behind the held owner"
    );
    assert!(matches!(report.outcome, StepOutcome::Failed { .. }));

    // Every started tool settled, and none of them as a success.
    let terminals = owner.tool_terminals();
    assert_eq!(terminals.len(), 2, "both tools must be settled");
    for (call, outcome) in &terminals {
        assert!(
            !outcome.is_success(),
            "{call:?} settled as success despite an interrupt"
        );
    }

    // Ordering: the one Step terminal comes after every tool terminal.
    let step_index = match owner.attempt_terminal_index() {
        Some(index) => index,
        None => panic!("an interrupted attempt must still write one attempt terminal"),
    };
    let last_tool = owner
        .committed
        .iter()
        .rposition(|fact| matches!(fact, DurableFact::ToolTerminal { .. }));
    if let Some(last_tool) = last_tool {
        assert!(
            step_index > last_tool,
            "the attempt terminal must be written after all tool terminals"
        );
    }
    assert_eq!(owner.attempt_terminal_count(), 1);

    // Queued fragments were explicitly classified, never silently dropped.
    let discarded = owner
        .observations
        .iter()
        .filter(|observation| matches!(observation, Observation::DiscardedOnCancel { .. }))
        .count();
    assert_eq!(
        discarded, report.diagnostics.discarded_on_cancel,
        "every discarded fragment must be observed"
    );
}

// ---------------------------------------------------------------------------
// Witness 4 — hung provider, no tool running, cancels promptly
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hung_provider_with_no_tool_running_cancels_promptly_and_never_succeeds() {
    // THE ORACLE RED BASELINE this corrects (provider-interrupt-midstream
    // corrected expectation): "interrupting a HUNG PROVIDER STREAM (no tool
    // running) does not settle promptly in the oracle — the proven
    // prompt-settlement interrupt path runs through running local tools."
    //
    // Here cancellation reaches the reader directly, so no tool is needed.
    let entered = Arc::new(Notify::new());
    let reader_entered = Arc::clone(&entered);
    let (interrupt, run) = prepare_admitted(
        attempt("att_hung"),
        move || HungReader {
            entered: reader_entered,
        },
        Arc::new(InstantTools),
        RecordingOwner::new(),
        residency(),
    );
    let handle = tokio::spawn(run);

    // Interrupt once the attempt is waiting on the hung reader.
    signalled(&entered, "the attempt never waited on its provider reader").await;
    interrupt.interrupt(AttemptCancelCause::Interrupted {
        by: InterruptionSource::User,
    });

    let (owner, report) = match tokio::time::timeout(Duration::from_secs(2), handle).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => panic!("attempt task panicked: {error}"),
        Err(_) => panic!("the oracle's defect reproduced: a hung provider did not cancel promptly"),
    };

    assert!(report.diagnostics.cancelled);
    assert_eq!(report.diagnostics.tools_started, 0, "no tool was involved");
    match report.outcome {
        // Typed, and specifically NOT `ProviderFailure { NoProgressTimeout }`:
        // the string carrier settled a user stop and a watchdog stall as the
        // same `aborted`, so this assertion could not tell them apart.
        StepOutcome::Failed { failure } => assert_eq!(
            failure.reason(),
            &AttemptTerminalReason::Interrupted {
                by: InterruptionSource::User
            }
        ),
        other => panic!("an interrupted attempt must never report success, got {other:?}"),
    }
    assert_eq!(owner.attempt_terminal_count(), 1);
}

// ---------------------------------------------------------------------------
