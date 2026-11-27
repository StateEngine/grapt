use super::*;
use grapt_subagent::{
    FollowupCancelReason, FollowupDecision, FollowupReceipt, FollowupStart, FollowupState,
};
use grapt_types::ExecutionId;

fn finished_peer(store: &mut EventStore) -> TestResult {
    SubagentService::start_on(store, &op("peer"), RuntimeGeneration::new(1), 10)?;
    SubagentService::settle_on(
        store,
        &op("peer"),
        &completed("the first task's answer")?,
        12,
    )?;
    Ok(())
}

fn queue(
    store: &mut EventStore,
    call: &str,
    target: &str,
    text: &str,
    generation: u64,
) -> Result<FollowupReceipt, grapt_subagent::SubagentError> {
    SubagentService::queue_followup_on(
        store,
        &AgentMessageRequest {
            sender: &ses("ses_sender"),
            target: &ses(target),
            call: &ToolCallId::new(call).unwrap_or_else(|| panic!("call identity")),
            text,
            created_ms: 3,
        },
        RuntimeGeneration::new(generation),
    )
}

fn start(
    store: &mut EventStore,
    receipt: &FollowupReceipt,
    name: &str,
    generation: u64,
) -> Result<FollowupDecision, grapt_subagent::SubagentError> {
    let mut next = admission(
        name,
        "ses_peer",
        "ses_mail_parent",
        SubagentMode::Background,
    );
    next.tool_call = None;
    SubagentService::claim_followup_on(
        store,
        &FollowupStart {
            message: &receipt.message,
            generation: RuntimeGeneration::new(generation),
            admission: &next,
            execution: &ExecutionId::new(format!("exe_{name}"))
                .unwrap_or_else(|| panic!("execution identity")),
            input: &msg(&format!("msg_wake_{name}")),
            input_event: &ev(&format!("evt_wake_{name}")),
            created_ms: 40,
        },
    )
}

fn operations(store: &EventStore) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(store
        .connection()
        .query_row("SELECT COUNT(*) FROM subagent_operation", [], |row| {
            row.get(0)
        })?)
}

#[test]
fn a_followup_creates_an_immutable_successor_and_preserves_the_original_parent() -> TestResult {
    let mut store = mail_store()?;
    family(&mut store)?;
    finished_peer(&mut store)?;
    let before: (String, Option<String>, i64) = store.connection().query_row(
        "SELECT status,output,time_updated FROM subagent_operation WHERE operation_id='peer'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    manifest_tool(&mut store, "call_next", "ses_sender", "followup_task")?;
    let receipt = queue(
        &mut store,
        "call_next",
        "ses_peer",
        "continue the review",
        1,
    )?;
    let FollowupDecision::Started { run, new_input, .. } = start(&mut store, &receipt, "next", 1)?
    else {
        panic!("followup did not receive execution custody")
    };
    assert!(new_input);
    assert_eq!(run.child, ses("ses_peer"));
    assert_eq!(run.operation, op("next"));
    let context = SubagentService::child_context_on(&store, &ses("ses_peer"))?.ok_or("child")?;
    assert_eq!(context.operation, run.operation);
    assert_eq!(context.parent, ses("ses_mail_parent"));
    assert_eq!(
        context.status,
        OperationStatus::Running(RuntimeGeneration::new(1))
    );
    let after: (String, Option<String>, i64) = store.connection().query_row(
        "SELECT status,output,time_updated FROM subagent_operation WHERE operation_id='peer'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(before, after, "followup changed the prior terminal result");
    assert_eq!(
        SubagentService::followup_run_on(&store, &run.operation)?,
        Some(run.clone())
    );
    assert_eq!(
        SubagentService::followup_cause_on(&store, &run.operation)?,
        Some(receipt.message.clone())
    );
    assert_eq!(
        start(&mut store, &receipt, "duplicate", 1)?,
        FollowupDecision::AlreadyStarted
    );
    assert_eq!(operations(&store)?, 4);
    let page = SubagentService::list_children_on(&store, &ses("ses_mail_parent"), None)?;
    let peers: Vec<_> = page
        .children
        .iter()
        .filter(|child| child.child == ses("ses_peer"))
        .collect();
    assert_eq!(
        peers.len(),
        1,
        "successive runs duplicated the child listing"
    );
    assert_eq!(peers[0].operation, run.operation);
    Ok(())
}

#[test]
fn a_captured_followup_never_restarts_a_settled_child() -> TestResult {
    let mut store = mail_store()?;
    family(&mut store)?;
    finished_peer(&mut store)?;
    manifest_tool(&mut store, "call_captured", "ses_sender", "followup_task")?;
    let receipt = queue(&mut store, "call_captured", "ses_peer", "use this note", 1)?;
    let batch =
        OperationMailBatch::prepare(&store, &ses("ses_peer"), StepNumber::new(1).ok_or("step")?)?
            .ok_or("mail")?;
    capture_mail(&mut store, &batch, "ses_peer", 1, true, true)?;
    assert_eq!(
        start(&mut store, &receipt, "unnecessary", 1)?,
        FollowupDecision::Consumed
    );
    assert_eq!(operations(&store)?, 3);
    assert!(
        grapt_inbox::Inbox::pending_inputs_within(store.connection(), &ses("ses_peer"))?.is_empty()
    );
    assert_eq!(
        SubagentService::followup_on(&store, &receipt.message)?
            .ok_or("intent")?
            .state,
        FollowupState::Consumed
    );
    Ok(())
}

#[test]
fn unstarted_followups_advance_generation_and_cancel_without_replaying_work() -> TestResult {
    let mut store = mail_store()?;
    family(&mut store)?;
    finished_peer(&mut store)?;
    manifest_tool(&mut store, "call_recover", "ses_sender", "followup_task")?;
    let receipt = queue(&mut store, "call_recover", "ses_peer", "next task", 2)?;
    assert_eq!(
        SubagentService::next_generation_on(&store)?,
        RuntimeGeneration::new(3)
    );
    assert_eq!(
        SubagentService::cancel_stale_followups_on(&mut store, RuntimeGeneration::new(3), 30)?,
        1
    );
    assert_eq!(
        start(&mut store, &receipt, "resurrected", 3)?,
        FollowupDecision::Cancelled(FollowupCancelReason::ProcessLoss)
    );
    assert_eq!(operations(&store)?, 3);
    let latest = SubagentService::latest_followup_on(&store, &ses("ses_peer"))?
        .ok_or("visible cancellation")?;
    assert_eq!(
        latest.state,
        FollowupState::Cancelled(FollowupCancelReason::ProcessLoss)
    );
    Ok(())
}

#[test]
fn send_message_manifests_and_root_targets_cannot_authorize_followup() -> TestResult {
    let mut store = mail_store()?;
    family(&mut store)?;
    finished_peer(&mut store)?;
    manifest(&mut store, "call_queue_only", "ses_sender")?;
    assert!(queue(&mut store, "call_queue_only", "ses_peer", "not a wake", 1).is_err());
    manifest_tool(&mut store, "call_root", "ses_sender", "followup_task")?;
    assert!(queue(
        &mut store,
        "call_root",
        "ses_mail_parent",
        "cannot wake the root",
        1
    )
    .is_err());
    manifest_tool(
        &mut store,
        "call_foreign_root",
        "ses_sender",
        "followup_task",
    )?;
    assert!(queue(
        &mut store,
        "call_foreign_root",
        "ses_mail_foreign",
        "outside the family",
        1
    )
    .is_err());
    assert_eq!(count(&store)?, 0);
    Ok(())
}

#[test]
fn followup_retries_reconcile_and_changed_retries_never_change_the_wake() -> TestResult {
    let mut store = mail_store()?;
    family(&mut store)?;
    finished_peer(&mut store)?;
    manifest_tool(
        &mut store,
        "call_retry_followup",
        "ses_sender",
        "followup_task",
    )?;
    let receipt = queue(&mut store, "call_retry_followup", "ses_peer", "one task", 1)?;
    assert!(receipt.admitted);
    let again = queue(&mut store, "call_retry_followup", "ses_peer", "one task", 1)?;
    assert!(!again.admitted);
    assert_eq!(again.message, receipt.message);
    assert!(queue(
        &mut store,
        "call_retry_followup",
        "ses_peer",
        "a different task",
        1
    )
    .is_err());
    assert!(queue(
        &mut store,
        "call_retry_followup",
        "ses_mail_parent",
        "one task",
        1
    )
    .is_err());
    assert_eq!(count(&store)?, 1);
    assert_eq!(operations(&store)?, 3);
    Ok(())
}

#[test]
fn a_retained_unpromoted_input_is_reused_without_another_work_admission() -> TestResult {
    let mut inbox = grapt_inbox::Inbox::new(mail_store()?)?;
    family(inbox.store_mut())?;
    finished_peer(inbox.store_mut())?;
    let peer = ses("ses_peer");
    let input = msg("msg_retained_start");
    let event = ev("evt_retained_start");
    inbox.admit_synthetic(grapt_inbox::SyntheticAdmitRequest {
        event_id: &event,
        session: &peer,
        input: &input,
        text: "retained task",
        description: Some("retained start"),
        metadata: Some(&json!({"fixture":"retained"})),
        delivery: grapt_types::DeliveryMode::Queue,
        resume: false,
        created_ms: 2,
    })?;
    manifest_tool(
        inbox.store_mut(),
        "call_reuse",
        "ses_sender",
        "followup_task",
    )?;
    let receipt = queue(
        inbox.store_mut(),
        "call_reuse",
        "ses_peer",
        "continue it",
        1,
    )?;
    let FollowupDecision::Started {
        run,
        prompt,
        new_input,
    } = start(inbox.store_mut(), &receipt, "reuse", 1)?
    else {
        panic!("followup did not claim retained input")
    };
    assert!(!new_input);
    assert_eq!(run.input, input);
    assert_eq!(prompt, "retained task");
    let admitted: i64 = inbox.store().connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type='session.input.admitted.1'
         AND json_extract(data,'$.input.type')='synthetic'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        admitted, 1,
        "followup duplicated the prior unpromoted work input"
    );
    Ok(())
}

#[test]
fn wake_order_generation_and_original_parent_are_checked_before_any_write() -> TestResult {
    let mut store = mail_store()?;
    family(&mut store)?;
    finished_peer(&mut store)?;
    for call in ["call_fifo_first", "call_fifo_second"] {
        manifest_tool(&mut store, call, "ses_sender", "followup_task")?;
    }
    let first = queue(&mut store, "call_fifo_first", "ses_peer", "first", 1)?;
    let second = queue(&mut store, "call_fifo_second", "ses_peer", "second", 1)?;
    assert_eq!(
        SubagentService::queued_followups_on(&store, &ses("ses_peer"))?,
        2
    );
    assert_eq!(
        SubagentService::next_followup_on(&store, &ses("ses_peer"))?
            .ok_or("next")?
            .message,
        first.message
    );
    assert!(start(&mut store, &first, "wrong_generation", 2).is_err());
    assert_eq!(
        start(&mut store, &second, "out_of_order", 1)?,
        FollowupDecision::Waiting
    );
    let before = operations(&store)?;
    let mut wrong = admission(
        "wrong_parent",
        "ses_peer",
        "ses_sender",
        SubagentMode::Background,
    );
    wrong.tool_call = None;
    assert!(SubagentService::claim_followup_on(
        &mut store,
        &FollowupStart {
            message: &first.message,
            generation: RuntimeGeneration::new(1),
            admission: &wrong,
            execution: &ExecutionId::new("exe_wrong_parent").ok_or("execution")?,
            input: &msg("msg_wrong_parent"),
            input_event: &ev("evt_wrong_parent"),
            created_ms: 40,
        }
    )
    .is_err());
    assert_eq!(operations(&store)?, before, "refusal created an operation");
    assert!(
        grapt_inbox::Inbox::pending_inputs_within(store.connection(), &ses("ses_peer"))?.is_empty(),
        "refusal admitted a work input"
    );
    assert_eq!(
        SubagentService::cancel_child_followups_on(&mut store, &ses("ses_peer"), 41)?,
        2
    );
    assert_eq!(
        SubagentService::queued_followups_on(&store, &ses("ses_peer"))?,
        0
    );
    assert_eq!(
        start(&mut store, &first, "after_interrupt", 1)?,
        FollowupDecision::Cancelled(FollowupCancelReason::Interrupted)
    );
    Ok(())
}

#[test]
fn a_new_runs_cause_bypasses_eight_older_messages_through_the_same_send_permit() -> TestResult {
    let mut store = mail_store()?;
    family(&mut store)?;
    finished_peer(&mut store)?;
    for n in 0..9 {
        let call = format!("call_old_mail_{n}");
        manifest(&mut store, &call, "ses_sender")?;
        send(&mut store, &call, "ses_peer", &format!("older message {n}"))?;
    }
    manifest_tool(
        &mut store,
        "call_priority_cause",
        "ses_sender",
        "followup_task",
    )?;
    let receipt = queue(
        &mut store,
        "call_priority_cause",
        "ses_peer",
        "priority-followup-cause",
        1,
    )?;
    assert!(matches!(
        start(&mut store, &receipt, "priority", 1)?,
        FollowupDecision::Started { .. }
    ));
    let child = ses("ses_peer");
    let step = StepNumber::new(1).ok_or("step")?;
    let normal = OperationMailBatch::prepare(&store, &child, step)?.ok_or("normal mail")?;
    assert!(!normal
        .contents()
        .iter()
        .any(|text| text.contains("priority-followup-cause")));
    let priority =
        OperationMailBatch::prepare_with_priority(&store, &child, step, Some(&receipt.message))?
            .ok_or("priority mail")?;
    assert_eq!(priority.contents().len(), 8);
    assert!(priority
        .contents()
        .iter()
        .any(|text| text.contains("priority-followup-cause")));
    assert!(OperationMailBatch::prepare_with_priority(
        &store,
        &ses("ses_sender"),
        step,
        Some(&receipt.message)
    )
    .is_err());
    assert!(
        capture_mail(&mut store, &priority, "ses_peer", 1, false, true).is_err(),
        "priority mail bypassed the request-sent permit"
    );
    capture_mail(&mut store, &priority, "ses_peer", 1, true, false)?;
    assert_eq!(
        presented_rows(&store, &child, None, None, u64::MAX, None)?.len(),
        0,
        "rolled-back priority capture became replayable"
    );
    capture_mail(&mut store, &priority, "ses_peer", 1, true, true)?;
    let rows = presented_rows(&store, &child, None, None, u64::MAX, None)?;
    assert_eq!(rows.len(), 8);
    assert!(rows
        .iter()
        .any(|(_, text)| text.contains("priority-followup-cause")));
    Ok(())
}

#[test]
fn followup_capacity_refuses_without_stranding_mail_or_a_work_input() -> TestResult {
    let mut store = mail_store()?;
    family(&mut store)?;
    finished_peer(&mut store)?;
    for n in 0..5 {
        let call = format!("call_capacity_{n}");
        manifest_tool(&mut store, &call, "ses_sender", "followup_task")?;
        let result = queue(&mut store, &call, "ses_peer", "bounded pending work", 1);
        assert_eq!(result.is_ok(), n < 4, "wrong wake capacity at {n}");
    }
    let sender = ses("ses_sender");
    let child = ses("ses_peer");
    let call = ToolCallId::new("call_capacity_0").ok_or("call")?;
    let request = AgentMessageRequest {
        sender: &sender,
        target: &child,
        call: &call,
        text: "bounded pending work",
        created_ms: 4,
    };
    let receipt =
        SubagentService::existing_followup_on(&store, &request)?.ok_or("existing receipt")?;
    assert!(
        !receipt.admitted,
        "retry tried to transfer another wake owner"
    );
    assert_eq!(receipt.state, FollowupState::Queued);
    let different = AgentMessageRequest {
        text: "changed retry",
        ..request
    };
    assert!(SubagentService::existing_followup_on(&store, &different).is_err());
    assert_eq!(count(&store)?, 4, "refused wake left agent mail behind");
    assert_eq!(
        SubagentService::queued_followups_on(&store, &ses("ses_peer"))?,
        4
    );
    assert!(
        grapt_inbox::Inbox::pending_inputs_within(store.connection(), &ses("ses_peer"))?.is_empty(),
        "queued wake prematurely admitted a work input"
    );
    assert_eq!(operations(&store)?, 3);
    Ok(())
}

#[test]
fn additive_schema_recovery_keeps_legacy_operations_and_queued_mail() -> TestResult {
    let mut store = mail_store()?;
    family(&mut store)?;
    finished_peer(&mut store)?;
    let peer = ses("ses_peer");
    let before = SubagentService::child_context_on(&store, &peer)?.ok_or("legacy child")?;
    let operation_count = operations(&store)?;
    // Only newly added, empty tables in this in-memory fixture are removed.
    store
        .connection()
        .execute_batch("DROP TABLE subagent_run; DROP TABLE subagent_followup;")?;
    let legacy = SubagentService::child_context_on(&store, &peer)?.ok_or("legacy disappeared")?;
    assert_eq!(legacy.operation, before.operation);
    assert_eq!(legacy.parent, before.parent);
    assert!(SubagentService::followup_on(&store, &msg("msg_absent_schema"))?.is_none());
    SubagentService::ensure_schema_on(&store)?;
    assert_eq!(operations(&store)?, operation_count);
    manifest_tool(
        &mut store,
        "call_schema_recovery",
        "ses_sender",
        "followup_task",
    )?;
    let receipt = queue(
        &mut store,
        "call_schema_recovery",
        "ses_peer",
        "retain this queued work",
        1,
    )?;
    let retained =
        SubagentService::followup_on(&store, &receipt.message)?.ok_or("queued intent")?;
    let event_count: i64 =
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))?;
    // A crash during additive creation can leave one of the two tables.
    // It is corruption to readers until startup completes the installation.
    store
        .connection()
        .execute_batch("DROP TABLE subagent_run;")?;
    let error = SubagentService::followup_on(&store, &receipt.message)
        .err()
        .ok_or("partial schema appeared to contain no work")?;
    assert!(
        error.to_string().contains("schema is incomplete"),
        "{error}"
    );
    SubagentService::ensure_schema_on(&store)?;
    assert_eq!(
        SubagentService::followup_on(&store, &receipt.message)?.ok_or("lost queued work")?,
        retained
    );
    assert_eq!(
        operations(&store)?,
        operation_count,
        "schema recovery started another run"
    );
    let after: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))?;
    assert_eq!(after, event_count, "schema recovery changed canonical mail");
    Ok(())
}
