//! Which partial output closes the retry window: a failure before visible
//! output retries the byte-identical request; a failure after it ends the Step.
use super::*;

/// SC-REL-02a: arguments still streaming are not visible output, so a
/// transport drop mid-arguments retries; no tool ran for the dropped call.
#[tokio::test]
async fn a_drop_while_tool_arguments_stream_retries_instead_of_ending_the_chain() {
    let registry = ModelRegistry::install(catalog("u2-rt-tool-input"));
    let Some(call) = grapt_types::ToolCallId::new("call_partial_arguments") else {
        panic!("the fixture call id is valid")
    };
    let partial = ScriptBuilder::new()
        .raw(grapt_provider::AttemptFragment::ToolInputOpened {
            call: call.clone(),
            tool: "read".into(),
            execution: grapt_provider::ToolExecution::Local,
            state: grapt_provider::AdapterState::none(),
        })
        .raw(grapt_provider::AttemptFragment::ToolInputDelta {
            call,
            tool: "read".into(),
            text: "{\"path\":\"src/".into(),
        })
        .fail(ProviderFailureClass::Transport, None, "connection reset");
    let opener = Arc::new(AttemptOpener::deterministic(
        [partial, transcript_script()],
        route(),
        credential(),
    ));
    let harness = harness(
        "ses_u2_tool_input_retry",
        opener,
        Box::new(RegistryPlan { registry }),
    );
    let (harness, report) = run(harness, policy(Duration::from_millis(2))).await;

    assert!(
        matches!(
            report.result,
            StepResult::Ended {
                finish: FinishReason::Stop
            }
        ),
        "{:?}",
        report.result
    );
    assert_eq!(report.receipts.len(), 2, "one retry, two physical attempts");
    assert_eq!(
        terminal_rows(&harness.inbox, &harness.session, "session.step.ended.3"),
        1
    );
    assert_eq!(
        terminal_rows(
            &harness.inbox,
            &harness.session,
            "session.tool.input.started.1"
        ) + terminal_rows(
            &harness.inbox,
            &harness.session,
            "session.tool.input.started.2"
        ),
        0,
        "the dropped call never started"
    );
}

/// SC-REL-02b: a drop mid-reasoning, before any block closed, is retried
/// instead of ending the Step; the second attempt's answer ends it.
#[tokio::test]
async fn a_drop_while_reasoning_streams_retries_instead_of_ending_the_step() {
    let registry = ModelRegistry::install(catalog("u2-rt-partial-reasoning"));
    let block = grapt_provider::BlockOrdinal::new(0);
    let partial = ScriptBuilder::new()
        .raw(grapt_provider::AttemptFragment::ReasoningOpened {
            block,
            state: grapt_provider::AdapterState::none(),
        })
        .raw(grapt_provider::AttemptFragment::ReasoningDelta {
            block,
            text: "weighing the".into(),
            state: grapt_provider::AdapterState::none(),
        })
        .fail(ProviderFailureClass::Transport, None, "connection reset");
    let opener = Arc::new(AttemptOpener::deterministic(
        [partial, transcript_script()],
        route(),
        credential(),
    ));
    let harness = harness(
        "ses_u2_partial_reasoning_retry",
        opener,
        Box::new(RegistryPlan { registry }),
    );
    let (_, report) = run(harness, policy(Duration::from_millis(2))).await;

    assert!(
        matches!(
            report.result,
            StepResult::Ended {
                finish: FinishReason::Stop
            }
        ),
        "{:?}",
        report.result
    );
    assert_eq!(report.receipts.len(), 2, "one retry, two physical attempts");
    assert!(!report.receipts[0].diagnostics.visible_output);
}

/// SC-REL-02c: finished reasoning with no answer or tool call after it is
/// not visible output, so a drop still retries; the retry re-thinks.
#[tokio::test]
async fn a_drop_after_finished_reasoning_retries_instead_of_ending_the_step() {
    let registry = ModelRegistry::install(catalog("u2-rt-finished-reasoning"));
    let thought = ScriptBuilder::new()
        .reasoning(&["weighing ", "the options"])
        .fail(ProviderFailureClass::Transport, None, "connection reset");
    let opener = Arc::new(AttemptOpener::deterministic(
        [thought, transcript_script()],
        route(),
        credential(),
    ));
    let harness = harness(
        "ses_u2_finished_reasoning_retry",
        opener,
        Box::new(RegistryPlan { registry }),
    );
    let (_, report) = run(harness, policy(Duration::from_millis(2))).await;

    assert!(
        matches!(
            report.result,
            StepResult::Ended {
                finish: FinishReason::Stop
            }
        ),
        "{:?}",
        report.result
    );
    assert_eq!(report.receipts.len(), 2, "one retry, two physical attempts");
    assert!(!report.receipts[0].diagnostics.visible_output);
}

/// SC-REL-02c: an owner that committed output early (finished reasoning past
/// its hold bound) is a retry barrier across the real Step driver and both
/// owner wrappers: the drop that follows stops the Step, never retried from a
/// history that now carries the committed reasoning.
#[tokio::test]
async fn an_owner_that_committed_output_stops_the_step_instead_of_retrying() {
    let registry = ModelRegistry::install(catalog("u2-rt-committed-output"));
    let thought = ScriptBuilder::new().reasoning(&["weighing"]).fail(
        ProviderFailureClass::Transport,
        None,
        "connection reset",
    );
    let opener = Arc::new(AttemptOpener::deterministic(
        [thought, transcript_script()],
        route(),
        credential(),
    ));
    let harness = harness_owned(
        "ses_u2_committed_output",
        opener,
        Box::new(RegistryPlan { registry }),
        Arc::new(TestClock),
        RecordingOwner::committing_reasoning_early(),
    );
    let (_, report) = run(harness, policy(Duration::from_millis(2))).await;

    match &report.result {
        StepResult::Failed { failure } => assert!(
            matches!(
                failure.reason(),
                StepTerminalReason::ProviderFailure {
                    class: TerminalClass::Transport
                }
            ),
            "{:?}",
            failure.reason()
        ),
        other => panic!("committed output must stop the Step: {other:?}"),
    }
    assert_eq!(report.receipts.len(), 1, "no retry past committed output");
    assert!(report.receipts[0].diagnostics.visible_output);
}
