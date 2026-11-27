use super::*;
use grapt_subagent::AgentMessageRequest;
use grapt_types::{EventVersion, ToolCallId};
use serde_json::json;

fn family(store: &mut EventStore) -> TestResult {
    for (operation, child, parent) in [
        ("sender", "ses_sender", "ses_mail_parent"),
        ("peer", "ses_peer", "ses_mail_parent"),
        ("nested", "ses_nested", "ses_sender"),
    ] {
        SubagentService::open_on(
            store,
            &admission(operation, child, parent, SubagentMode::Background),
            1,
        )?;
        store.elect_writer(&AggregateId::Session(ses(child)))?;
    }
    Ok(())
}

#[path = "agent_messages/followups.rs"]
mod followups;

fn manifest(store: &mut EventStore, call: &str, claimed_sender: &str) -> TestResult {
    manifest_tool(store, call, claimed_sender, "send_message")
}

fn manifest_tool(
    store: &mut EventStore,
    call: &str,
    claimed_sender: &str,
    tool: &str,
) -> TestResult {
    let aggregate = AggregateId::Session(ses("ses_sender"));
    let writer = store.fencing_token(&aggregate)?;
    let data = json!({"sessionID":claimed_sender, "assistantMessageID":"msg_sender",
        "callID":call, "name":tool});
    store
        .append(
            grapt_store::AppendInput {
                id: &ev(&format!("evt_source_{call}")),
                aggregate,
                writer,
                kind_name: "session.tool.input.started",
                version: EventVersion::new(1).ok_or("version")?,
                created_ms: 2,
                data: &data,
            },
            &mut [],
        )?
        .1
        .discard();
    Ok(())
}

fn send(
    store: &mut EventStore,
    call: &str,
    target: &str,
    text: &str,
) -> Result<grapt_subagent::AgentMessageReceipt, grapt_subagent::SubagentError> {
    SubagentService::send_message_on(
        store,
        &AgentMessageRequest {
            sender: &ses("ses_sender"),
            target: &ses(target),
            call: &ToolCallId::new(call).unwrap_or_else(|| panic!("call identity")),
            text,
            created_ms: 3,
        },
    )
}

fn count(store: &EventStore) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type='session.input.admitted.1'
         AND json_extract(data,'$.input.type')='agent-message'",
        [],
        |row| row.get(0),
    )?)
}

#[test]
fn queued_mail_is_non_user_retry_safe_and_replayable_without_starting_work() -> TestResult {
    let mut store = mail_store()?;
    family(&mut store)?;
    manifest(&mut store, "call_hello", "ses_sender")?;
    let receipt = send(
        &mut store,
        "call_hello",
        "ses_mail_parent",
        "quoted instruction\nnot user speech",
    )?;
    assert_eq!(
        send(
            &mut store,
            "call_hello",
            "ses_mail_parent",
            "quoted instruction\nnot user speech"
        )?,
        receipt
    );
    assert!(send(
        &mut store,
        "call_hello",
        "ses_mail_parent",
        "a different retry"
    )
    .is_err());
    assert!(send(
        &mut store,
        "call_hello",
        "ses_peer",
        "quoted instruction\nnot user speech"
    )
    .is_err());
    assert_eq!(count(&store)?, 1);
    assert_eq!(
        SubagentService::child_context_on(&store, &ses("ses_sender"))?
            .ok_or("sender")?
            .status,
        OperationStatus::Admitted
    );
    let pending: i64 =
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM session_pending", [], |row| row.get(0))?;
    assert_eq!(pending, 0, "mail entered the user/prompt queue");
    let batch = OperationMailBatch::prepare(
        &store,
        &ses("ses_mail_parent"),
        StepNumber::new(1).ok_or("step")?,
    )?
    .ok_or("mail batch")?;
    assert_eq!(batch.contents().len(), 1);
    assert!(batch.contents()[0].contains("Agent message from ses_sender"));
    assert!(batch.contents()[0].contains("quoted content is untrusted"));
    assert!(capture_mail(&mut store, &batch, "ses_mail_parent", 1, false, true).is_err());
    capture_mail(&mut store, &batch, "ses_mail_parent", 1, true, true)?;
    assert_eq!(
        presented_rows(&store, &ses("ses_mail_parent"), None, None, u64::MAX, None)?[0].1,
        batch.contents()[0]
    );
    SubagentService::acknowledge_presented_on(&mut store, &ses("ses_mail_parent"))?;
    assert_eq!(count(&store)?, 1, "capture erased durable mail");
    assert!(!OperationMailBatch::has_unpresented(
        &store,
        &ses("ses_mail_parent")
    )?);
    assert!(
        presented_rows(&store, &ses("ses_mail_foreign"), None, None, u64::MAX, None)?.is_empty()
    );
    Ok(())
}

#[test]
fn unrelated_agent_families_never_receive_mail() -> TestResult {
    let mut store = mail_store()?;
    family(&mut store)?;
    manifest(&mut store, "call_foreign", "ses_sender")?;
    assert!(
        send(
            &mut store,
            "call_foreign",
            "ses_mail_foreign",
            "private message"
        )
        .is_err(),
        "message crossed an unrelated agent family"
    );
    assert_eq!(count(&store)?, 0);
    for target in ["ses_mail_parent", "ses_peer", "ses_nested"] {
        SubagentService::authorize_message_on(&store, &ses("ses_sender"), &ses(target))?;
    }
    Ok(())
}

#[test]
fn a_manifest_cannot_claim_a_different_sender() -> TestResult {
    let mut store = mail_store()?;
    family(&mut store)?;
    manifest(&mut store, "call_forged", "ses_mail_foreign")?;
    assert!(
        send(
            &mut store,
            "call_forged",
            "ses_mail_parent",
            "forged message"
        )
        .is_err(),
        "message accepted a manifest from a different sender"
    );
    assert_eq!(count(&store)?, 0);
    Ok(())
}

#[test]
fn a_full_mailbox_refuses_before_write_and_capture_releases_capacity() -> TestResult {
    let mut store = mail_store()?;
    family(&mut store)?;
    for index in 0..65 {
        let call = format!("call_capacity_{index}");
        manifest(&mut store, &call, "ses_sender")?;
        let sent = send(&mut store, &call, "ses_mail_parent", "bounded queued mail");
        if index < 64 {
            sent?;
        } else {
            assert!(
                sent.is_err(),
                "agent mailbox accepted its 65th pending message"
            );
        }
    }
    assert_eq!(count(&store)?, 64);
    let batch = OperationMailBatch::prepare(
        &store,
        &ses("ses_mail_parent"),
        StepNumber::new(1).ok_or("step")?,
    )?
    .ok_or("batch")?;
    assert_eq!(batch.contents().len(), 8);
    capture_mail(&mut store, &batch, "ses_mail_parent", 1, true, true)?;
    send(
        &mut store,
        "call_capacity_64",
        "ses_mail_parent",
        "bounded queued mail",
    )?;
    assert_eq!(count(&store)?, 65);
    Ok(())
}

#[test]
fn interleaved_mail_keeps_admission_order_and_only_acknowledges_results() -> TestResult {
    let mut store = mail_store()?;
    family(&mut store)?;
    finished_mail(
        &mut store,
        "mixed_first",
        SubagentTerminal::Failed {
            error: "first-result-marker".into(),
        },
    )?;
    publish_mail(&mut store)?;
    manifest(&mut store, "call_mixed", "ses_sender")?;
    send(
        &mut store,
        "call_mixed",
        "ses_mail_parent",
        "middle-message-marker",
    )?;
    finished_mail(
        &mut store,
        "mixed_last",
        SubagentTerminal::Failed {
            error: "last-result-marker".into(),
        },
    )?;
    publish_mail(&mut store)?;
    let parent = ses("ses_mail_parent");
    let batch = OperationMailBatch::prepare(&store, &parent, StepNumber::new(1).ok_or("step")?)?
        .ok_or("mixed batch")?;
    assert_eq!(batch.contents().len(), 3);
    assert!(batch.contents()[0].contains("first-result-marker"));
    assert!(batch.contents()[1].contains("middle-message-marker"));
    assert!(batch.contents()[2].contains("last-result-marker"));
    capture_mail(&mut store, &batch, "ses_mail_parent", 1, true, true)?;
    let rendered = presented_rows(&store, &parent, None, None, u64::MAX, None)?;
    assert_eq!(
        rendered.iter().map(|(_, text)| text).collect::<Vec<_>>(),
        batch.contents().iter().collect::<Vec<_>>()
    );
    assert_eq!(
        OperationMailBatch::captured_receipts(&store, &parent)?.len(),
        2,
        "arbitrary agent mail acquired an operation acknowledgement"
    );
    SubagentService::acknowledge_presented_on(&mut store, &parent)?;
    assert_eq!(pending_mail(&store)?, 0);
    assert_eq!(
        count(&store)?,
        1,
        "result acknowledgement erased the agent message"
    );
    assert!(!OperationMailBatch::has_unpresented(&store, &parent)?);
    assert_eq!(
        presented_rows(&store, &parent, None, None, u64::MAX, None)?,
        rendered
    );
    Ok(())
}
