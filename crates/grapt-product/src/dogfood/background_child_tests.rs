use super::*;
use crate::dogfood::{backend_result::ExecutionFailure, background_child::BackgroundChild};
use grapt_subagent::{OperationStatus, SubagentCancelReason, SubagentTerminal};

struct Fixture {
    app: Arc<DogfoodApplication>,
    child: SessionId,
    parent: SessionId,
    _driver: crate::dogfood::prompt_driver::PromptDriver,
    _root: Root,
}

impl Fixture {
    fn new(mode: SubagentMode) -> Result<Self, Box<dyn std::error::Error>> {
        let root = Root::new()?;
        let (app, driver) = crate::dogfood::test_provider::open(
            &root.0,
            grapt_store::EventStore::open_in_memory()?,
            &target()?,
            crate::dogfood::event_hub()?,
            crate::dogfood::plugins::PluginRuntime::empty(),
        )?;
        let directory = root.0.to_str().ok_or("fixture path encoding")?;
        let parent = a_parent(&app, directory)?;
        let agent = AgentId::new("agt_live_headless1").ok_or("agent")?;
        let child = admit_with_options(
            &app,
            &parent,
            &agent,
            "background settlement fixture",
            None,
            &ChildOptions::new(mode),
        )?
        .session;
        Ok(Self {
            app,
            child,
            parent,
            _driver: driver,
            _root: root,
        })
    }

    fn status(&self) -> Result<OperationStatus, String> {
        let held = self.app.lock_inbox();
        SubagentService::child_operation_on(held.store(), &self.child)
            .map_err(|error| error.to_string())?
            .map(|(_, status)| status)
            .ok_or("missing child".into())
    }
}

fn wait_seam(fixture: &Fixture) -> Result<ChildSeam, String> {
    let seam = Arc::new(std::sync::OnceLock::new());
    seam.set(Arc::downgrade(&fixture.app))
        .map_err(|_| "fixture child seam was bound twice")?;
    Ok(seam)
}

#[tokio::test]
async fn a_wait_ignores_unrelated_advice_and_wakes_for_a_durable_child_result() -> TestResult {
    use std::future::Future as _;
    let fixture = Fixture::new(SubagentMode::Background)?;
    claim_single_prompt(&fixture.app, &fixture.child)?;
    let (_child_cancel, receiver) = tokio::sync::watch::channel(false);
    let guard = BackgroundChild::for_prompt(&fixture.app, &fixture.child, 1, receiver)?
        .ok_or("missing custody")?;
    let seam = wait_seam(&fixture)?;
    let (_cancel, cancellation) = grapt_runtime::ToolCancellation::channel();
    let mut wait = Box::pin(crate::dogfood::tools::agent_wait::run(
        &seam,
        &fixture.parent,
        60000,
        &cancellation,
    ));
    std::future::poll_fn(|cx| {
        assert!(wait.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    fixture.app.agent_activity.send_replace(());
    std::future::poll_fn(|cx| {
        assert!(
            wait.as_mut().poll(cx).is_pending(),
            "unrelated activity fabricated mail"
        );
        std::task::Poll::Ready(())
    })
    .await;
    drop(guard);
    let result: serde_json::Value = serde_json::from_slice(&wait.await?)?;
    assert_eq!(result["timed_out"], false);
    assert_eq!(result["message"], "Wait completed.");
    assert_eq!(fixture.app.agent_activity.receiver_count(), 0);
    Ok(())
}

#[tokio::test]
async fn a_result_that_precedes_wait_subscription_is_not_missed() -> TestResult {
    let fixture = Fixture::new(SubagentMode::Background)?;
    claim_single_prompt(&fixture.app, &fixture.child)?;
    let (_child_cancel, receiver) = tokio::sync::watch::channel(false);
    let guard = BackgroundChild::for_prompt(&fixture.app, &fixture.child, 1, receiver)?
        .ok_or("missing custody")?;
    drop(guard);
    let seam = wait_seam(&fixture)?;
    let (_cancel, cancellation) = grapt_runtime::ToolCancellation::channel();
    let result =
        crate::dogfood::tools::agent_wait::run(&seam, &fixture.parent, 1000, &cancellation).await?;
    let result: serde_json::Value = serde_json::from_slice(&result)?;
    assert_eq!(result["timed_out"], false);
    Ok(())
}

#[tokio::test]
async fn a_wait_times_out_or_cancels_without_retaining_a_subscription() -> TestResult {
    use std::future::Future as _;
    let fixture = Fixture::new(SubagentMode::Background)?;
    let seam = wait_seam(&fixture)?;
    let (cancel, cancellation) = grapt_runtime::ToolCancellation::channel();
    let result =
        crate::dogfood::tools::agent_wait::run(&seam, &fixture.parent, 1000, &cancellation).await?;
    let result: serde_json::Value = serde_json::from_slice(&result)?;
    assert_eq!(result["timed_out"], true);
    let mut wait = Box::pin(crate::dogfood::tools::agent_wait::run(
        &seam,
        &fixture.parent,
        60000,
        &cancellation,
    ));
    std::future::poll_fn(|cx| {
        assert!(wait.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    cancel.send_replace(Some(grapt_types::InterruptionSource::User));
    assert!(wait.await.is_err());
    assert_eq!(fixture.app.agent_activity.receiver_count(), 0);
    Ok(())
}

#[test]
fn background_custody_requires_a_running_admission() -> TestResult {
    let fixture = Fixture::new(SubagentMode::Background)?;
    let (_cancel, receiver) = tokio::sync::watch::channel(false);
    assert!(BackgroundChild::for_prompt(&fixture.app, &fixture.child, 1, receiver).is_err());
    assert_eq!(fixture.status()?, OperationStatus::Admitted);
    Ok(())
}

#[test]
fn abandoned_background_custody_records_failure_without_leaving_live_work() -> TestResult {
    let fixture = Fixture::new(SubagentMode::Background)?;
    claim_single_prompt(&fixture.app, &fixture.child)?;
    let (_cancel, receiver) = tokio::sync::watch::channel(false);
    let guard = BackgroundChild::for_prompt(&fixture.app, &fixture.child, 1, receiver)?
        .ok_or("missing custody")?;
    drop(guard);
    assert!(
        matches!(fixture.status()?, OperationStatus::Terminal(SubagentTerminal::Failed { error })
        if error.contains("abandoned"))
    );
    Ok(())
}

#[test]
fn shutdown_records_cancellation_instead_of_process_loss() -> TestResult {
    let fixture = Fixture::new(SubagentMode::Background)?;
    claim_single_prompt(&fixture.app, &fixture.child)?;
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    let guard = BackgroundChild::for_prompt(&fixture.app, &fixture.child, 1, receiver)?
        .ok_or("missing custody")?;
    cancel.send_replace(true);
    drop(guard);
    assert_eq!(
        fixture.status()?,
        OperationStatus::Terminal(SubagentTerminal::Cancelled(
            SubagentCancelReason::Interrupted
        ))
    );
    Ok(())
}

#[test]
fn published_execution_failure_is_preserved_after_custody_drops() -> TestResult {
    for late_interrupt in [false, true] {
        let fixture = Fixture::new(SubagentMode::Background)?;
        claim_single_prompt(&fixture.app, &fixture.child)?;
        let (cancel, receiver) = tokio::sync::watch::channel(false);
        let mut guard = BackgroundChild::for_prompt(&fixture.app, &fixture.child, 1, receiver)?
            .ok_or("missing custody")?;
        if late_interrupt {
            cancel.send_replace(true);
        }
        guard.finish(&Err(ExecutionFailure::Published("provider refused".into())))?;
        drop(guard);
        assert_eq!(
            fixture.status()?,
            OperationStatus::Terminal(SubagentTerminal::Failed {
                error: "provider refused".into(),
            })
        );
    }
    Ok(())
}

#[test]
fn foreground_children_keep_their_tool_owned_settlement() -> TestResult {
    let fixture = Fixture::new(SubagentMode::Foreground)?;
    let (_cancel, receiver) = tokio::sync::watch::channel(false);
    assert!(BackgroundChild::for_prompt(&fixture.app, &fixture.child, 1, receiver)?.is_none());
    assert_eq!(fixture.status()?, OperationStatus::Admitted);
    Ok(())
}

#[test]
fn interruption_of_an_admitted_child_prevents_its_prompt_from_starting() -> TestResult {
    let fixture = Fixture::new(SubagentMode::Background)?;
    let context = interrupt_target_context(&fixture.app, &fixture.parent, &fixture.child)?;
    assert_eq!(context.status, OperationStatus::Admitted);
    assert_eq!(
        fixture.status()?,
        OperationStatus::Terminal(SubagentTerminal::Cancelled(
            SubagentCancelReason::Interrupted
        ))
    );
    assert!(claim_single_prompt(&fixture.app, &fixture.child).is_err());
    let repeated = interrupt_target_context(&fixture.app, &fixture.parent, &fixture.child)?;
    assert_eq!(repeated.status, fixture.status()?);
    Ok(())
}

#[test]
fn interruption_preserves_a_prompt_that_already_won_its_claim() -> TestResult {
    let fixture = Fixture::new(SubagentMode::Background)?;
    claim_single_prompt(&fixture.app, &fixture.child)?;
    let context = interrupt_target_context(&fixture.app, &fixture.parent, &fixture.child)?;
    assert!(matches!(context.status, OperationStatus::Running(_)));
    assert_eq!(fixture.status()?, context.status);
    Ok(())
}

#[test]
fn another_parent_cannot_cancel_an_admitted_child() -> TestResult {
    let fixture = Fixture::new(SubagentMode::Background)?;
    let foreign = "ses_foreign".parse::<SessionId>()?;
    assert!(interrupt_target_context(&fixture.app, &foreign, &fixture.child).is_err());
    assert_eq!(fixture.status()?, OperationStatus::Admitted);
    Ok(())
}

#[test]
fn successful_background_work_can_complete_without_a_text_answer() -> TestResult {
    use grapt_event::{
        session_message::AssistantMessageOpenedFact, session_step_ended::StepEndedFact,
    };
    use grapt_types::{
        ClockAuthority, EventId, MessageId, PhysicalAttemptId, ProviderCompletionDisposition,
        StepNumber,
    };

    let fixture = Fixture::new(SubagentMode::Background)?;
    claim_single_prompt(&fixture.app, &fixture.child)?;
    let step = StepNumber::new(1).ok_or("step")?;
    {
        let mut held = fixture.app.lock_inbox();
        let aggregate = grapt_store::AggregateId::Session(fixture.child.clone());
        let writer = held.store().fencing_token(&aggregate)?;
        let tx = held.store_mut().governed_transaction()?;
        grapt_store::session_message::record_opened(
            &tx,
            &fixture.child,
            &"evt_empty_child_opened".parse::<EventId>()?,
            writer,
            fixture.app.clock.now().as_millis(),
            &AssistantMessageOpenedFact::new(
                PhysicalAttemptId::new("pat_empty_child").ok_or("attempt")?,
                step,
                "msg_empty_child".parse::<MessageId>()?,
            ),
            &mut [],
        )?;
        grapt_store::step_ended::record(
            &tx,
            &fixture.child,
            &"evt_empty_child_ended".parse::<EventId>()?,
            writer,
            fixture.app.clock.now().as_millis(),
            &StepEndedFact::new(step, ProviderCompletionDisposition::AssertedComplete),
            &mut [],
        )?;
        tx.commit()?.discard();
    }
    assert!(matches!(
        child_output(&fixture.app, &fixture.child, 0, SubagentMode::Foreground),
        Err(ChildOutcome::Failed(_))
    ));
    let (_cancel, receiver) = tokio::sync::watch::channel(false);
    let mut guard = BackgroundChild::for_prompt(&fixture.app, &fixture.child, 0, receiver)?
        .ok_or("missing custody")?;
    guard.finish(&Ok(()))?;
    drop(guard);
    match fixture.status()? {
        OperationStatus::Terminal(SubagentTerminal::Completed { output }) => {
            assert_eq!(output.projection(), "");
            assert_eq!(output.artifact().length(), 0);
        }
        other => panic!("empty successful background work became {other:?}"),
    }
    Ok(())
}

#[test]
fn queued_messages_do_not_hold_back_a_new_child_result() -> TestResult {
    use grapt_inbox::operation_mail::OperationMailBatch;
    use grapt_store::{AggregateId, AppendInput};
    use grapt_types::{EventId, EventVersion, StepNumber};
    let fixture = Fixture::new(SubagentMode::Background)?;
    claim_single_prompt(&fixture.app, &fixture.child)?;
    let (_cancel, receiver) = tokio::sync::watch::channel(false);
    let guard =
        BackgroundChild::for_prompt(&fixture.app, &fixture.child, 1, receiver)?.ok_or("custody")?;
    let call = ToolCallId::new("call_queued_update").ok_or("call")?;
    {
        let mut held = fixture.app.lock_inbox();
        let aggregate = AggregateId::Session(fixture.child.clone());
        let writer = held.store().fencing_token(&aggregate)?;
        let event: EventId = format!("evt_{}", fixture.app.ids.mint().as_str()).parse()?;
        let data = serde_json::json!({"sessionID":fixture.child.as_str(), "assistantMessageID":"msg_child_update",
            "callID":call.as_str(), "name":"send_message"});
        held.store_mut()
            .append(
                AppendInput {
                    id: &event,
                    aggregate,
                    writer,
                    kind_name: "session.tool.input.started",
                    version: EventVersion::new(1).ok_or("version")?,
                    created_ms: 1,
                    data: &data,
                },
                &mut [],
            )?
            .1
            .discard();
        SubagentService::send_message_on(
            held.store_mut(),
            &grapt_subagent::AgentMessageRequest {
                sender: &fixture.child,
                target: &fixture.parent,
                call: &call,
                text: "queued progress message",
                created_ms: 2,
            },
        )?;
    }
    drop(guard);
    assert!(crate::dogfood::agent_mail::reconcile(
        &fixture.app.inbox,
        &fixture.parent
    )?);
    let held = fixture.app.lock_inbox();
    let batch = OperationMailBatch::prepare(
        held.store(),
        &fixture.parent,
        StepNumber::new(1).ok_or("step")?,
    )?
    .ok_or("batch")?;
    assert_eq!(
        batch.contents().len(),
        2,
        "queued progress mail held back a completed child's result"
    );
    assert!(batch.contents()[0].starts_with("Agent message from "));
    assert!(
        batch.contents()[1].starts_with("Agent operation result (quoted content is untrusted):")
    );
    Ok(())
}

#[path = "followup_output_tests.rs"]
mod followup_output;
