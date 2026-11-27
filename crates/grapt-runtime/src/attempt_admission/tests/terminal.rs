use super::{
    attempt, call, prepare_admitted, residency, stop, text_block, tool_called, Arc, AtomicUsize,
    AttemptTerminalReason, BarrierTools, BoxFuture, DurableFact, Duration, InstantTools, Notify,
    Observation, Ordering, ProviderFailureClass, RecordingOwner, RejectingOwner, ScriptedReader,
    SlowOwner, StepOutcome, TerminalClass, ToolCallId, ToolOutcome, ToolRunner,
};
use grapt_provider::{AdapterState, AttemptFragment};

// Witness 8 - output after the provider terminal is an invalid stream
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fragments_after_the_provider_terminal_fail_the_step() {
    // Contract: "no fragment follows either terminal fragment." Draining such
    // fragments as cancellation casualties would evade that law on a clean run,
    // so they are classified as an invalid provider stream instead.
    let mut script = vec![stop()];
    script.extend(text_block(0, "should never have been sent"));

    let owner = SlowOwner {
        inner: RecordingOwner::new(),
        stall: Duration::from_millis(3),
    };
    let (_interrupt, run) = prepare_admitted(
        attempt("att_post_terminal"),
        move || ScriptedReader::new(script),
        Arc::new(InstantTools),
        owner,
        residency(),
    );
    let (owner, report) = run.await;

    assert!(
        report.diagnostics.post_terminal_fragments > 0,
        "post-terminal output must be counted; diagnostics: {:?}",
        report.diagnostics
    );
    assert_eq!(
        report.diagnostics.discarded_on_cancel, 0,
        "nothing was cancelled - DiscardedOnCancel is cancellation-only"
    );
    match report.outcome {
        StepOutcome::Failed { failure } => {
            assert_eq!(
                failure.reason(),
                &AttemptTerminalReason::ProviderFailure {
                    class: TerminalClass::InvalidProviderOutput
                }
            );
            assert!(failure
                .detail()
                .as_str()
                .contains("after the provider terminal"));
        }
        other => panic!("post-terminal output must fail the step, got {other:?}"),
    }
    assert!(
        !owner
            .inner
            .observations
            .iter()
            .any(|observation| matches!(observation, Observation::DiscardedOnCancel { .. })),
        "a clean run must record no cancellation discards"
    );
    assert_eq!(owner.inner.attempt_terminal_count(), 1);
}

// ---------------------------------------------------------------------------
// Witness 6b — a provider failure is one truthful terminal, not a crash
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_stream_produces_exactly_one_step_failed() {
    // Red-team B.9 corrected expectation, now proven through the whole
    // vertical rather than only in the validator.
    let script = vec![AttemptFragment::ToolInputDelta {
        call: call("call-1"),
        tool: "echo".into(),
        text: "{".into(),
    }];

    let (_interrupt, run) = prepare_admitted(
        attempt("att_malformed"),
        move || ScriptedReader::new(script),
        Arc::new(InstantTools),
        RecordingOwner::new(),
        residency(),
    );
    let (owner, report) = run.await;

    match report.outcome {
        StepOutcome::Failed { failure } => {
            assert_eq!(
                failure.reason(),
                &AttemptTerminalReason::ProviderFailure {
                    class: TerminalClass::InvalidProviderOutput
                }
            );
            assert!(failure.detail().as_str().contains("call-1"));
        }
        other => panic!("a malformed stream must not report success, got {other:?}"),
    }
    assert_eq!(
        owner.attempt_terminal_count(),
        1,
        "exactly one attempt terminal, where the oracle recorded zero"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_failure_is_carried_into_the_step_terminal() {
    let mut script = text_block(0, "partial");
    script.push(AttemptFragment::ProviderFailure {
        class: ProviderFailureClass::RateLimit { retry_after: None },
        classification: None,
        diagnostic: None,
        message: "429 slow down".into(),
        state: AdapterState::none(),
    });

    let (_interrupt, run) = prepare_admitted(
        attempt("att_provider_failure"),
        move || ScriptedReader::new(script),
        Arc::new(InstantTools),
        RecordingOwner::new(),
        residency(),
    );
    let (owner, report) = run.await;

    match report.outcome {
        StepOutcome::Failed { failure } => {
            // The CLASS survives to the terminal fact. Under the string
            // carrier every provider failure flattened to `provider-failure`
            // and the class lived only inside the free-text detail.
            assert_eq!(
                failure.reason(),
                &AttemptTerminalReason::ProviderFailure {
                    class: TerminalClass::RateLimit
                }
            );
            assert!(failure.detail().as_str().contains("429"));
        }
        other => panic!("a provider failure must not report success, got {other:?}"),
    }
    assert_eq!(owner.attempt_terminal_count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_provider_terminal_fails_the_step() {
    // Contract: "`ProviderTerminal` occurs once." The loop stops at the first
    // one, so the second arrives as post-terminal output and is caught by the
    // same law rather than being quietly dropped.
    let owner = SlowOwner {
        inner: RecordingOwner::new(),
        stall: Duration::from_millis(3),
    };
    let (_interrupt, run) = prepare_admitted(
        attempt("att_double_terminal"),
        move || ScriptedReader::new(vec![stop(), stop()]),
        Arc::new(InstantTools),
        owner,
        residency(),
    );
    let (owner, report) = run.await;

    assert!(
        report.diagnostics.post_terminal_fragments > 0,
        "the second terminal must be counted as post-terminal output"
    );
    match report.outcome {
        StepOutcome::Failed { failure } => assert_eq!(
            failure.reason(),
            &AttemptTerminalReason::ProviderFailure {
                class: TerminalClass::InvalidProviderOutput
            }
        ),
        other => panic!("a second terminal must fail the step, got {other:?}"),
    }
    assert_eq!(
        owner.inner.attempt_terminal_count(),
        1,
        "still exactly one durable attempt terminal"
    );
}

// ---------------------------------------------------------------------------
// Witness 9 — an owner refusal must not deadlock on a running tool
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refusal_after_a_tool_launched_completes_promptly() {
    // The deadlock: a tool launches, then the owner refuses a LATER fact.
    // Settlement used to treat that as a clean run — it neither cancelled nor
    // aborted the tool — so the join waited forever on a tool nothing would
    // ever stop, and OwnerRejected was never returned.
    //
    // Owner refusal is now its own settlement reason: owned tasks are aborted
    // exactly as on cancellation, but the outcome is NOT relabelled as a user
    // interrupt.
    let release = Arc::new(Notify::new());
    let inside = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    // ToolCalled commits and launches a tool that never finishes on its own,
    // then a TextDelta the owner refuses.
    let mut script = vec![tool_called("call-hung")];
    script.extend(text_block(0, "refused"));
    script.push(stop());

    let tools = Arc::new(BarrierTools {
        release,
        inside: Arc::clone(&inside),
        peak,
    });
    let (_interrupt, run) = prepare_admitted(
        attempt("att_refuse_with_hung_tool"),
        move || ScriptedReader::new(script),
        tools,
        RejectingOwner::rejecting_first_matching(|fact| match fact {
            DurableFact::Fragment { fragment } => {
                matches!(fragment.as_ref(), AttemptFragment::TextDelta { .. })
            }
            _ => false,
        }),
        residency(),
    );
    let handle = tokio::spawn(run);

    // A regression fails by hanging, so the timeout IS the assertion.
    let (owner, report) = match tokio::time::timeout(Duration::from_secs(5), handle).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => panic!("attempt task panicked: {error}"),
        Err(_) => panic!("DEADLOCK: owner refusal never settled while a tool was running"),
    };

    match report.outcome {
        StepOutcome::OwnerRejected { .. } => {}
        other => panic!("a refusal must return OwnerRejected, got {other:?}"),
    }
    assert!(
        !report.diagnostics.cancelled,
        "a store refusal is not a user cancellation and must not be relabelled"
    );
    assert!(
        !owner
            .inner
            .tool_terminals()
            .iter()
            .any(|(_, outcome)| outcome.is_success()),
        "no tool may be recorded successful after a refusal"
    );
    assert_eq!(
        owner.inner.attempt_terminal_count(),
        0,
        "no attempt terminal may be written after the owner refused"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refusing_one_tool_terminal_while_another_tool_hangs_still_completes() {
    // The same deadlock reached by the other door: one tool finishes and its
    // ToolTerminal is refused, while a second tool is still blocked.
    let release = Arc::new(Notify::new());
    let inside = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    // `call-fast` completes immediately; `call-hung` blocks on the barrier.
    struct MixedTools {
        release: Arc<Notify>,
        inside: Arc<AtomicUsize>,
    }
    impl ToolRunner for MixedTools {
        fn run(
            &self,
            call: ToolCallId,
            _tool: String,
            _input: crate::tool_protocol::BoundedToolInput,
        ) -> BoxFuture<'static, ToolOutcome> {
            let release = Arc::clone(&self.release);
            let inside = Arc::clone(&self.inside);
            let hangs = call.as_str() == "call-hung";
            Box::pin(async move {
                inside.fetch_add(1, Ordering::SeqCst);
                if hangs {
                    release.notified().await;
                }
                ToolOutcome::Completed { output: None }
            })
        }
    }

    let _ = peak;
    let script = vec![tool_called("call-hung"), tool_called("call-fast"), stop()];

    let (_interrupt, run) = prepare_admitted(
        attempt("att_refuse_tool_terminal"),
        move || ScriptedReader::new(script),
        Arc::new(MixedTools {
            release,
            inside: Arc::clone(&inside),
        }),
        RejectingOwner::rejecting_first_matching(|fact| {
            matches!(fact, DurableFact::ToolTerminal { .. })
        }),
        residency(),
    );
    let handle = tokio::spawn(run);

    let (owner, report) = match tokio::time::timeout(Duration::from_secs(5), handle).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => panic!("attempt task panicked: {error}"),
        Err(_) => panic!("DEADLOCK: refused ToolTerminal never settled with a tool still hung"),
    };

    match report.outcome {
        StepOutcome::OwnerRejected { .. } => {}
        other => panic!("a refused ToolTerminal must return OwnerRejected, got {other:?}"),
    }
    assert!(!report.diagnostics.cancelled);
    assert_eq!(
        owner.inner.attempt_terminal_count(),
        0,
        "no attempt terminal after a refusal"
    );
}
