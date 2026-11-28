use super::*;
use grapt_store::{AggregateId, AppendInput};
use grapt_types::{EventId, EventVersion};
use serde_json::{json, Value};

fn source_call(
    application: &ServerApplication,
    parent: &SessionId,
) -> Result<ToolCallId, Box<dyn std::error::Error>> {
    let mut held = application.lock_inbox();
    let aggregate = AggregateId::Session(parent.clone());
    let writer = held.store().fencing_token(&aggregate)?;
    let rows = [
        (
            "session.input.admitted",
            json!({"sessionID":parent.as_str(), "inputID":"msg_parent_instruction",
            "input":{"type":"user", "data":{"text":"parent-history-marker"}, "delivery":"queue"}}),
        ),
        (
            "session.input.promoted",
            json!({"sessionID":parent.as_str(), "inputID":"msg_parent_instruction"}),
        ),
        (
            "session.assistant-message.opened",
            json!({"message":"msg_spawning", "attempt":"attempt_spawn", "step":1}),
        ),
        (
            "session.tool.input.started",
            json!({"sessionID":parent.as_str(), "assistantMessageID":"msg_spawning",
            "callID":"call_spawn", "name":"spawn_agent"}),
        ),
    ];
    for (kind_name, data) in rows {
        let event: EventId = format!("evt_{}", application.ids.mint().as_str()).parse()?;
        let (_, notices) = held.store_mut().append(
            AppendInput {
                id: &event,
                aggregate: aggregate.clone(),
                writer,
                kind_name,
                version: EventVersion::new(1).ok_or("version")?,
                created_ms: application.clock.now().as_millis(),
                data: &data,
            },
            &mut [&mut grapt_inbox::InboxProjector],
        )?;
        notices.discard();
    }
    ToolCallId::new("call_spawn").ok_or_else(|| "call".into())
}

#[test]
fn forked_admission_preserves_the_permission_ceiling_mode_placement_and_requested_agent(
) -> TestResult {
    let root = Root::new()?;
    let (app, _driver) = crate::dogfood::test_provider::open(
        &root.0,
        grapt_store::EventStore::open_in_memory()?,
        &target()?,
        crate::dogfood::event_hub()?,
        crate::dogfood::plugins::PluginRuntime::empty(),
    )?;
    let directory = root.0.to_str().ok_or("path")?;
    let parent = a_parent(&app, directory)?;
    let authorities = DeterministicAuthorities::new(&*app.clock, &*app.ids);
    grapt_session::selection::select_preset_committed(
        app.lock_inbox().store_mut(),
        &authorities,
        &parent,
        Some("inspect"),
        &mut [&mut grapt_session::SessionListProjector],
    )?;
    let call = source_call(&app, &parent)?;
    let agent = AgentId::new("reviewer").ok_or("agent")?;
    let admitted = admit_with_options(
        &app,
        &parent,
        &agent,
        "review_changes",
        Some(call.clone()),
        &ChildOptions::new(SubagentMode::Background).with_history_before(call),
    )?;
    let child = app.get_record(&admitted.session)?;
    assert_eq!(child.parent.as_ref(), Some(&parent));
    assert_eq!(child.placement.directory(), directory);
    assert_eq!(child.agent.as_deref(), Some(agent.as_str()));
    assert_eq!(child.title, "review_changes");
    assert_eq!(
        child.preset.as_deref(),
        Some("inspect"),
        "fork escaped its parent's permission ceiling"
    );
    let held = app.lock_inbox();
    let context =
        SubagentService::child_context_on(held.store(), &admitted.session)?.ok_or("child")?;
    assert_eq!(context.mode, SubagentMode::Background);
    assert_eq!(context.parent, parent);
    let raw: String = held.store().connection().query_row(
        "SELECT data FROM event WHERE aggregate_id=?1 AND type='session.input.admitted.1'",
        [admitted.session.as_str()],
        |row| row.get(0),
    )?;
    let input: Value = serde_json::from_str(&raw)?;
    assert_eq!(input["input"]["data"]["text"], "parent-history-marker");
    assert_ne!(input["inputID"], "msg_parent_instruction");
    assert!(
        held.pending_inputs(&admitted.session)?.is_empty(),
        "fork copied the parent's input queue"
    );
    Ok(())
}

#[test]
fn a_history_source_must_match_the_admitting_tool_call_before_any_child_is_written() -> TestResult {
    let root = Root::new()?;
    let (app, _driver) = crate::dogfood::test_provider::open(
        &root.0,
        grapt_store::EventStore::open_in_memory()?,
        &target()?,
        crate::dogfood::event_hub()?,
        crate::dogfood::plugins::PluginRuntime::empty(),
    )?;
    let parent = a_parent(&app, root.0.to_str().ok_or("path")?)?;
    let call = source_call(&app, &parent)?;
    let result = admit_with_options(
        &app,
        &parent,
        &AgentId::new("reviewer").ok_or("agent")?,
        "review_changes",
        Some(ToolCallId::new("call_other").ok_or("call")?),
        &ChildOptions::new(SubagentMode::Background).with_history_before(call),
    );
    assert!(
        result.is_err(),
        "child admission accepted a different history source"
    );
    let forks: i64 = app.lock_inbox().store().connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type='session.forked.2'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(forks, 0, "invalid source left a child history");
    Ok(())
}

#[test]
fn historical_operation_mail_does_not_become_a_child_receipt_or_wake() -> TestResult {
    let root = Root::new()?;
    let (app, _driver) = crate::dogfood::test_provider::open(
        &root.0,
        grapt_store::EventStore::open_in_memory()?,
        &target()?,
        crate::dogfood::event_hub()?,
        crate::dogfood::plugins::PluginRuntime::empty(),
    )?;
    let parent = a_parent(&app, root.0.to_str().ok_or("path")?)?;
    present_historical_result(&app, &parent)?;
    let expected = {
        let held = app.lock_inbox();
        let rows = grapt_inbox::operation_mail::presented_rows(
            held.store(),
            &parent,
            None,
            None,
            u64::MAX,
            None,
        )?;
        match rows.as_slice() {
            [(_, text)] => text.clone(),
            _ => return Err("fixture must contain exactly one presented result".into()),
        }
    };
    let call = source_call(&app, &parent)?;
    let admitted = admit_with_options(
        &app,
        &parent,
        &AgentId::new("explorer").ok_or("agent")?,
        "read_history",
        Some(call.clone()),
        &ChildOptions::new(SubagentMode::Background).with_history_before(call),
    )?;
    let held = app.lock_inbox();
    assert!(
        held.pending_inputs(&admitted.session)?.is_empty(),
        "fork replayed mail as work"
    );
    let (text, kind): (String, String) = held.store().connection().query_row(
        "SELECT text,type FROM session_message WHERE session_id=?1 ORDER BY seq LIMIT 1",
        [admitted.session.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(kind, "synthetic");
    assert_eq!(text, expected, "fork changed the parent mail cache prefix");
    assert!(
        text.contains("historical-result-marker") && text.contains("quoted content is untrusted"),
        "{text}"
    );
    let receipts: i64 = held.store().connection().query_row(
        "SELECT COUNT(*) FROM event WHERE aggregate_family='session' AND aggregate_id=?1
         AND type='session.subagent-mail.presented.1'",
        [admitted.session.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(
        receipts, 0,
        "fork fabricated a delivery receipt for its parent"
    );
    Ok(())
}

fn present_historical_result(app: &ServerApplication, parent: &SessionId) -> TestResult {
    use grapt_store::{AttemptRecordCeiling, ReserveInput, TerminalCoordinates};
    use grapt_types::{
        BoundedAttemptId, BoundedSessionId, DurableAttemptReceipt, PhysicalAttemptId,
        RequestSentMarker, ReservedMarker, StepNumber,
    };
    let mut held = app.lock_inbox();
    let aggregate = AggregateId::Session(parent.clone());
    let writer = held.store().fencing_token(&aggregate)?;
    let id: EventId = format!("evt_{}", app.ids.mint().as_str()).parse()?;
    let (appended, notices) = held.store_mut().append(
        AppendInput {
            id: &id,
            aggregate: aggregate.clone(),
            writer,
            kind_name: "session.input.admitted",
            version: EventVersion::new(1).ok_or("version")?,
            created_ms: app.clock.now().as_millis(),
            data: &json!({"sessionID":parent.as_str(),"inputID":"msg_parent_result",
        "input":{"type":"operation-delivery","operationID":"opr_parent_result",
            "subagentOperationID":"sub_parent_result","claimID":"claim_parent_result",
            "generation":1,"settlement":"failed","projection":"historical-result-marker"}}),
        },
        &mut [],
    )?;
    notices.discard();
    let source = match appended {
        grapt_store::Appended::Committed { seq } | grapt_store::Appended::Idempotent { seq } => seq,
    };
    let attempt = PhysicalAttemptId::new("attempt_parent_result").ok_or("attempt")?;
    let step = StepNumber::new(1).ok_or("step")?;
    let maximum = std::num::NonZeroU32::new(1024 * 1024).ok_or("record bound")?;
    let ceiling = AttemptRecordCeiling::new(maximum);
    let now = app.clock.now();
    let receipt = DurableAttemptReceipt::reserved(
        BoundedAttemptId::new(attempt.clone(), maximum).map_err(|error| format!("{error:?}"))?,
        BoundedSessionId::new(parent.clone(), maximum).map_err(|error| format!("{error:?}"))?,
        step,
        grapt_types::AttemptOrdinal::new(1).ok_or("ordinal")?,
        ReservedMarker::new(now),
    );
    let terminal = TerminalCoordinates::new(
        format!("evt_{}", app.ids.mint().as_str()).parse()?,
        now.as_millis(),
    );
    let tx = held.store_mut().governed_transaction()?;
    grapt_store::EventStore::reserve_attempt_receipt_within(
        &tx,
        &ReserveInput {
            receipt: &receipt,
            writer,
            terminal: &terminal,
            ceiling,
        },
    )?;
    grapt_store::EventStore::permit_attempt_send_within(
        &tx,
        &attempt,
        writer,
        ceiling,
        RequestSentMarker::new(now),
    )?;
    let id: EventId = "evt_agent_mail_msg_parent_result".parse()?;
    grapt_store::EventStore::append_operation_mail_presentation_within(
        &tx,
        AppendInput {
            id: &id,
            aggregate,
            writer,
            kind_name: "session.subagent-mail.presented",
            version: EventVersion::new(1).ok_or("version")?,
            created_ms: now.as_millis(),
            data: &json!({"sessionID":parent.as_str(),"inputID":"msg_parent_result",
        "sourceSeq":source,"attemptID":attempt.as_str(),"step":step.value()}),
        },
        ceiling,
    )?;
    tx.commit()?.discard();

    Ok(())
}

#[tokio::test]
async fn history_fork_uses_the_prepared_detached_worktree() -> TestResult {
    let root = Root::new()?;
    super::worktree::commit_source(&root.0)?;
    std::fs::write(root.0.join("nested/file.txt"), "parent edit")?;
    let (app, _driver) = crate::dogfood::test_provider::open(
        &root.0,
        grapt_store::EventStore::open_in_memory()?,
        &target()?,
        crate::dogfood::event_hub()?,
        crate::dogfood::plugins::PluginRuntime::empty(),
    )?;
    let source = root.0.join("nested");
    let parent = a_parent(&app, source.to_str().ok_or("path")?)?;
    let call = source_call(&app, &parent)?;
    let options = ChildOptions::new(SubagentMode::Background)
        .with_history_before(call.clone())
        .with_worktree(&app, &parent)
        .await?;
    let admitted = admit_with_options(
        &app,
        &parent,
        &AgentId::new("reviewer").ok_or("agent")?,
        "isolated_history",
        Some(call),
        &options,
    )?;
    let child = app.get_record(&admitted.session)?;
    let directory = std::path::Path::new(child.placement.directory());
    assert_eq!(child.parent.as_ref(), Some(&parent));
    assert!(directory.ends_with("nested"));
    assert_ne!(directory, source);
    assert_eq!(
        std::fs::read_to_string(directory.join("file.txt"))?,
        "committed"
    );
    assert_eq!(
        std::fs::read_to_string(source.join("file.txt"))?,
        "parent edit"
    );
    let held = app.lock_inbox();
    let inputs = held.promoted_messages(&admitted.session, 128)?;
    assert!(inputs
        .iter()
        .any(|input| input.text == "parent-history-marker"));
    assert!(held.pending_inputs(&admitted.session)?.is_empty());
    Ok(())
}
