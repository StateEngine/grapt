// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: bounded native wake custody and immutable run bindings.
//! Followup uses the existing bounded prompt driver and Session wake custody.
use super::*;
use grapt_subagent::{
    AgentMessageRequest, FollowupCancelReason, FollowupDecision, FollowupReceipt, FollowupStart,
    FollowupState, SubagentAdmission, SubagentMode, SubagentService,
};
use grapt_types::{EffectiveGraphId, OperationId, SubagentOperationId, ToolCallId};

struct RunIds {
    operation: OperationId,
    subagent: SubagentOperationId,
    graph: EffectiveGraphId,
    input: MessageId,
    input_event: EventId,
    execution: ExecutionId,
}

impl RunIds {
    fn mint(app: &DogfoodApplication) -> Result<Self, String> {
        Ok(Self {
            operation: OperationId::new(format!("opr_{}", app.ids.mint().as_str()))
                .ok_or("operation identity")?,
            subagent: SubagentOperationId::new(format!("sub_{}", app.ids.mint().as_str()))
                .ok_or("subagent identity")?,
            graph: EffectiveGraphId::new(format!("efg_{}", app.ids.mint().as_str()))
                .ok_or("graph identity")?,
            input: format!("msg_{}", app.ids.mint().as_str())
                .parse()
                .map_err(|error| format!("input identity: {error}"))?,
            input_event: app.mint_event()?,
            execution: app.mint_execution()?,
        })
    }
}

pub(super) fn receipt_json(receipt: &FollowupReceipt) -> Value {
    let (status, operation, reason) = match &receipt.state {
        FollowupState::Queued => ("queued", None, None),
        FollowupState::Started(operation) => ("started", Some(operation.as_str()), None),
        FollowupState::Consumed => ("delivered", None, None),
        FollowupState::Cancelled(reason) => ("cancelled", None, Some(reason.as_str())),
    };
    json!({"message_id":receipt.message.as_str(),"status":status,
        "operation_id":operation,"reason":reason})
}

fn response(child: &SessionId, receipt: &FollowupReceipt) -> Result<Vec<u8>, String> {
    let mut value = receipt_json(receipt);
    value["agent_id"] = json!(child.as_str());
    serde_json::to_vec(&value).map_err(|error| error.to_string())
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn submit(
    app: &Arc<DogfoodApplication>,
    sender: &SessionId,
    child: &SessionId,
    principal: &SealedPrincipal,
    call: &ToolCallId,
    text: &str,
) -> Result<Vec<u8>, String> {
    let request = AgentMessageRequest {
        sender,
        target: child,
        call,
        text,
        created_ms: app.clock.now().as_millis(),
    };
    {
        let held = app.lock_inbox();
        SubagentService::authorize_message_on(held.store(), sender, child)
            .map_err(|error| error.to_string())?;
        SubagentService::child_context_on(held.store(), child)
            .map_err(|error| error.to_string())?
            .ok_or("followup cannot target the root agent")?;
        if let Some(receipt) = SubagentService::existing_followup_on(held.store(), &request)
            .map_err(|error| error.to_string())?
        {
            return response(child, &receipt);
        }
    }
    let record = app.get_record(child)?;
    let reservation = app.prompt_mailbox.reserve()?;
    let wake = app
        .wake_slots
        .reserve_for_workspace(child, Path::new(record.placement.directory()))?;
    let ids = RunIds::mint(app)?;
    app.ensure_session_writer(child)?;
    let receipt = SubagentService::queue_followup_on(
        app.lock_inbox().store_mut(),
        &request,
        app.runtime_generation,
    )
    .map_err(|error| error.to_string())?;
    if receipt.admitted {
        let (cancel, cancelled) = watch::channel(false);
        let wake_cancelled = wake.cancellation();
        let pending = PendingFollowup {
            app: Arc::clone(app),
            message: receipt.message.clone(),
            child: child.clone(),
            execution: ids.execution.clone(),
            cancelled: cancelled.clone(),
            wake_cancelled: wake_cancelled.clone(),
            armed: true,
        };
        let cancel_app = Arc::clone(app);
        let cancel_message = receipt.message.clone();
        let cancel_child = child.clone();
        let cancel_execution = ids.execution.clone();
        let cancel_work = Arc::new(move |reason| {
            let _settled = cancel_app
                .executor
                .shutdown(&cancel_child, &cancel_execution, reason);
            cancel.send_replace(true);
            if let Err(error) = cancel_queued(
                &cancel_app,
                &cancel_message,
                FollowupCancelReason::Interrupted,
            ) {
                cancel_app
                    .hub
                    .invalidate_because(&format!("followup cancellation failed: {error}"));
            }
        });
        let failed_app = Arc::clone(app);
        let failed_message = receipt.message.clone();
        let failed_child = child.clone();
        let failed_execution = ids.execution.clone();
        let failed_cancelled = cancelled.clone();
        let failed_wake_cancelled = wake_cancelled.clone();
        let failed = Arc::new(move || {
            let reason = if *failed_cancelled.borrow() || *failed_wake_cancelled.borrow() {
                FollowupCancelReason::Interrupted
            } else {
                FollowupCancelReason::Unavailable
            };
            if let Err(error) = abandon(
                &failed_app,
                &failed_message,
                &failed_child,
                &failed_execution,
                reason,
            ) {
                failed_app
                    .hub
                    .invalidate_because(&format!("followup abandonment failed: {error}"));
            }
        });
        let application = Arc::clone(app);
        let target = child.clone();
        let principal = principal.clone();
        let message = receipt.message.clone();
        let completion = wake.completion();
        reservation.submit_shutdown(
            ids.execution.clone(),
            Box::pin(async move {
                let _completion = completion;
                if let Err(error) = drive(
                    &application,
                    &target,
                    &message,
                    principal,
                    ids,
                    wake,
                    cancelled,
                    wake_cancelled,
                    pending,
                )
                .await
                {
                    application
                        .hub
                        .invalidate_because(&format!("agent followup could not start: {error}"));
                }
            }),
            cancel_work,
            failed,
        );
        app.agent_activity.send_replace(());
    }
    response(child, &receipt)
}

fn cancel_queued(
    app: &DogfoodApplication,
    message: &MessageId,
    reason: FollowupCancelReason,
) -> Result<(), String> {
    SubagentService::cancel_followup_on(
        app.lock_inbox().store_mut(),
        message,
        app.runtime_generation,
        reason,
        app.clock.now().as_millis(),
    )
    .map_err(|error| error.to_string())?;
    app.agent_activity.send_replace(());
    Ok(())
}

fn abandon(
    app: &DogfoodApplication,
    message: &MessageId,
    child: &SessionId,
    execution: &ExecutionId,
    reason: FollowupCancelReason,
) -> Result<(), String> {
    let run = {
        let held = app.lock_inbox();
        match SubagentService::followup_on(held.store(), message)
            .map_err(|error| error.to_string())?
            .ok_or("owned followup has no durable intent")?
            .state
        {
            FollowupState::Started(operation) => {
                SubagentService::followup_run_on(held.store(), &operation)
                    .map_err(|error| error.to_string())?
            }
            _ => None,
        }
    };
    if let Some(run) = run {
        if run.child != *child || run.execution != *execution {
            return Err("followup lost its exact run binding".into());
        }
        let terminal = match reason {
            FollowupCancelReason::Interrupted => grapt_subagent::SubagentTerminal::Cancelled(
                grapt_subagent::SubagentCancelReason::Interrupted,
            ),
            FollowupCancelReason::ProcessLoss => grapt_subagent::SubagentTerminal::Cancelled(
                grapt_subagent::SubagentCancelReason::ProcessLoss,
            ),
            FollowupCancelReason::Unavailable => grapt_subagent::SubagentTerminal::Failed {
                error: "the owned followup execution was abandoned".into(),
            },
        };
        subagent::settle(app, &run.operation, &terminal)?;
        if reason == FollowupCancelReason::Unavailable {
            app.executor.0.observe_abandoned(child, execution);
        }
    } else {
        cancel_queued(app, message, reason)?;
    }
    Ok(())
}

struct PendingFollowup {
    app: Arc<DogfoodApplication>,
    message: MessageId,
    child: SessionId,
    execution: ExecutionId,
    cancelled: watch::Receiver<bool>,
    wake_cancelled: watch::Receiver<bool>,
    armed: bool,
}

impl Drop for PendingFollowup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let reason = if *self.cancelled.borrow() || *self.wake_cancelled.borrow() {
            FollowupCancelReason::Interrupted
        } else {
            FollowupCancelReason::Unavailable
        };
        if let Err(error) = abandon(
            &self.app,
            &self.message,
            &self.child,
            &self.execution,
            reason,
        ) {
            self.app
                .hub
                .invalidate_because(&format!("owned followup cleanup failed: {error}"));
        }
    }
}

fn claim_with_custody(
    app: &DogfoodApplication,
    guard: &execution::WakeGuard,
    child: &SessionId,
    message: &MessageId,
    ids: &RunIds,
) -> Result<(FollowupDecision, grapt_types::AgentId), String> {
    guard.require_session(child)?;
    let record = app.get_record(child)?;
    let agent = grapt_types::AgentId::new(record.agent.unwrap_or_else(|| "default".into()))
        .ok_or("child agent identity")?;
    let model = app.executor.session_target(record.model.as_ref())?;
    let mut held = app.lock_inbox();
    let context = SubagentService::child_context_on(held.store(), child)
        .map_err(|error| error.to_string())?
        .ok_or("followup target is no longer a child")?;
    let admission = SubagentAdmission {
        operation: ids.operation.clone(),
        subagent: ids.subagent.clone(),
        child: child.clone(),
        parent: context.parent,
        agent: agent.clone(),
        model,
        description: "Agent followup".into(),
        mode: SubagentMode::Background,
        tool_call: None,
        effective_graph: ids.graph.clone(),
    };
    let decision = SubagentService::claim_followup_on(
        held.store_mut(),
        &FollowupStart {
            message,
            generation: app.runtime_generation,
            admission: &admission,
            execution: &ids.execution,
            input: &ids.input,
            input_event: &ids.input_event,
            created_ms: app.clock.now().as_millis(),
        },
    )
    .map_err(|error| error.to_string())?;
    Ok((decision, agent))
}

#[allow(
    clippy::too_many_arguments,
    reason = "one bounded followup custody transfer"
)]
async fn drive(
    app: &DogfoodApplication,
    child: &SessionId,
    message: &MessageId,
    principal: SealedPrincipal,
    ids: RunIds,
    wake: execution::WakeReservation,
    mut cancelled: watch::Receiver<bool>,
    mut wake_cancelled: watch::Receiver<bool>,
    mut pending: PendingFollowup,
) -> Result<(), String> {
    let mut activity = app.agent_activity.subscribe();
    loop {
        if *cancelled.borrow() || *wake_cancelled.borrow() {
            return Ok(());
        }
        let next = {
            let held = app.lock_inbox();
            SubagentService::next_followup_on(held.store(), child)
                .map_err(|error| error.to_string())?
        };
        if next.as_ref().is_some_and(|next| next.message == *message) {
            break;
        }
        let state = SubagentService::followup_on(app.lock_inbox().store(), message)
            .map_err(|error| error.to_string())?
            .ok_or("owned followup disappeared")?
            .state;
        if state != FollowupState::Queued {
            pending.armed = false;
            return Ok(());
        }
        tokio::select! {biased;
            _=cancelled.changed()=>return Ok(()),
            _=wake_cancelled.changed()=>return Ok(()),
            result=activity.changed()=>{result.map_err(|_|"followup activity owner closed")?;},
        }
    }
    let (started, _accepted) = prompt_driver::Started::channel();
    let Some(guard) = wake.enter(&started, cancelled.clone()).await? else {
        return Ok(());
    };
    loop {
        if *cancelled.borrow() || *wake_cancelled.borrow() {
            return Ok(());
        }
        let (decision, agent) = claim_with_custody(app, &guard, child, message, &ids)?;
        match decision {
            FollowupDecision::Waiting => tokio::select! {biased;
                _=cancelled.changed()=>return Ok(()),
                _=wake_cancelled.changed()=>return Ok(()),
                result=activity.changed()=>{result.map_err(|_|"followup activity owner closed")?;},
            },
            FollowupDecision::Consumed
            | FollowupDecision::Cancelled(_)
            | FollowupDecision::AlreadyStarted => {
                pending.armed = false;
                app.agent_activity.send_replace(());
                return Ok(());
            }
            FollowupDecision::Started {
                run,
                prompt: _,
                new_input,
            } => {
                let background = background_child::BackgroundChild::for_followup(
                    app,
                    &run,
                    cancelled.clone(),
                    wake_cancelled,
                );
                pending.armed = false;
                if new_input {
                    input_events::publish(&app.lock_inbox(), &app.hub, child, run.admitted_seq);
                }
                app.agent_activity.send_replace(());
                let (started, _accepted) = prompt_driver::Started::channel();
                app.execute_owned_prompt(
                    ExecutionRequest {
                        started,
                        wake: Some(execution::WakeCustody::Acquired(guard)),
                        external_notice: None,
                        session: run.child,
                        admission_seq: run.admitted_seq,
                        input: run.input,
                        interrupted: cancelled,
                        principal,
                        agent,
                        execution: run.execution,
                    },
                    Some(background),
                )
                .await;
                return Ok(());
            }
        }
    }
}
