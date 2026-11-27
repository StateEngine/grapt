use super::{
    attempt, prepare_admitted, residency, stop, text_block, tool_called, Arc, AtomicUsize,
    BarrierTools, DurableFact, Duration, FinishReason, InstantTools, Notify, Ordering,
    RecordingOwner, ScriptedReader, SlowOwner, StepOutcome, ATTEMPT_TOOL_BUDGET,
    PROVIDER_CHANNEL_CAPACITY,
};
use grapt_provider::AttemptFragment;

// Witness 1 — slow-owner backpressure, no loss, no reorder
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_owner_creates_bounded_backpressure_without_loss_or_reorder() {
    // Contract §"Bounded handoff and backpressure": "The reader awaits capacity
    // for non-coalescible fragments. This naturally slows the socket reader and
    // makes pressure visible."
    let mut script = Vec::new();
    for ordinal in 0..40u32 {
        script.extend(text_block(ordinal, &format!("chunk-{ordinal}")));
    }
    let expected_text: Vec<String> = (0..40u32).map(|n| format!("chunk-{n}")).collect();
    script.push(stop());

    let owner = SlowOwner {
        inner: RecordingOwner::new(),
        stall: Duration::from_micros(200),
    };
    let (_interrupt, run) = prepare_admitted(
        attempt("att_backpressure"),
        move || ScriptedReader::new(script),
        Arc::new(InstantTools),
        owner,
        residency(),
    );
    let (owner, report) = run.await;

    assert_eq!(
        report.outcome,
        StepOutcome::Ended {
            finish: FinishReason::Stop
        }
    );

    // Bounded: the queue never exceeded its named capacity.
    assert!(
        report.diagnostics.provider_high_water <= PROVIDER_CHANNEL_CAPACITY,
        "high water {} exceeded capacity {PROVIDER_CHANNEL_CAPACITY}",
        report.diagnostics.provider_high_water
    );
    // Visible: the producer actually waited, so this run really did apply
    // backpressure rather than trivially fitting in the buffer.
    // Contention, not elapsed time, is the proof. Timing every reserve would
    // count ordinary scheduler latency as backpressure.
    assert!(
        report.diagnostics.provider_contention_events > 0,
        "a stalled owner must fill the channel at least once; diagnostics: {:?}",
        report.diagnostics
    );
    assert!(
        report.diagnostics.producer_wait > Duration::ZERO,
        "a contended reserve must record measured wait time; diagnostics: {:?}",
        report.diagnostics
    );

    // No loss and no reorder: every delta is present, in script order.
    let seen: Vec<String> = owner
        .inner
        .committed
        .iter()
        .filter_map(|fact| match fact {
            DurableFact::Fragment { fragment } => match fragment.as_ref() {
                AttemptFragment::TextDelta { text, .. } => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(seen, expected_text, "fragments lost or reordered");
}

// ---------------------------------------------------------------------------
// Witness 2 — the 17th tool waits
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_seventeenth_tool_waits_for_a_permit() {
    // Measured max 17 local tool calls in one real step. AttemptToolBudget
    // is 16, so that one step waits exactly once — the disclosed bound.
    assert_eq!(ATTEMPT_TOOL_BUDGET, 16);

    let release = Arc::new(Notify::new());
    let inside = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let mut script = Vec::new();
    for n in 0..17 {
        script.push(tool_called(&format!("call-{n}")));
    }
    script.push(stop());

    let tools = Arc::new(BarrierTools {
        release: Arc::clone(&release),
        inside: Arc::clone(&inside),
        peak: Arc::clone(&peak),
    });

    let (_interrupt, run) = prepare_admitted(
        attempt("att_budget"),
        move || ScriptedReader::new(script),
        Arc::clone(&tools),
        RecordingOwner::new(),
        residency(),
    );
    let handle = tokio::spawn(run);

    // Watch the monotonic peak, not an instantaneous count: `inside` can be
    // sampled between a task acquiring its permit and registering itself, so
    // an exact equality on it is racy. `peak` only rises, so it cannot race.
    // The ceiling invariant is still checked on every sample.
    let mut observed_peak = 0usize;
    for _ in 0..400 {
        let live = inside.load(Ordering::SeqCst);
        assert!(
            live <= ATTEMPT_TOOL_BUDGET,
            "{live} tools ran at once, over the budget of {ATTEMPT_TOOL_BUDGET}"
        );
        observed_peak = peak.load(Ordering::SeqCst);
        if observed_peak == ATTEMPT_TOOL_BUDGET {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        observed_peak, ATTEMPT_TOOL_BUDGET,
        "the budget should saturate while the 17th tool waits"
    );

    // Release everyone; the 17th proceeds once a permit frees.
    for _ in 0..40 {
        release.notify_waiters();
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let (owner, report) = match handle.await {
        Ok(result) => result,
        Err(error) => panic!("attempt task panicked: {error}"),
    };

    assert_eq!(report.diagnostics.tools_started, 17);
    assert!(
        report.diagnostics.tools_peak_concurrent <= ATTEMPT_TOOL_BUDGET,
        "peak concurrency {} exceeded the budget",
        report.diagnostics.tools_peak_concurrent
    );
    assert!(
        peak.load(Ordering::SeqCst) <= ATTEMPT_TOOL_BUDGET,
        "the tools themselves observed more than the budget running"
    );
    assert_eq!(
        owner.tool_terminals().len(),
        17,
        "every started tool reaches exactly one terminal"
    );
    assert_eq!(owner.attempt_terminal_count(), 1);
}

// ---------------------------------------------------------------------------
