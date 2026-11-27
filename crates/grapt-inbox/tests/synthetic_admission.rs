use grapt_inbox::{
    AdmitOutcome, Inbox, InboxEvent, PromoteScope, PromotedInputKind, SyntheticAdmitRequest,
};
use grapt_store::{AggregateId, EventStore};
use grapt_types::{DeliveryMode, EventId, MessageId, SessionId};
use serde_json::json;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn request<'a>(
    event: &'a EventId,
    session: &'a SessionId,
    input: &'a MessageId,
    text: &'a str,
    metadata: &'a serde_json::Value,
) -> SyntheticAdmitRequest<'a> {
    SyntheticAdmitRequest {
        event_id: event,
        session,
        input,
        text,
        description: Some("background completion"),
        metadata: Some(metadata),
        delivery: DeliveryMode::Queue,
        resume: false,
        created_ms: 1_789_560_000_000,
    }
}

#[test]
fn authorized_synthetic_is_durable_exact_and_distinct_from_legacy_imports() -> TestResult {
    let session: SessionId = "ses_synthetic_witness".parse()?;
    let input: MessageId = "msg_synthetic_witness".parse()?;
    let mut store = EventStore::open_in_memory()?;
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    let mut inbox = Inbox::new(store)?;
    let metadata = json!({"job":"offline"});
    let event: EventId = "evt_synthetic_witness".parse()?;
    let first = inbox.admit_synthetic(request(&event, &session, &input, "completed", &metadata))?;
    assert!(matches!(first, AdmitOutcome::Admitted { .. }));
    let retry_event: EventId = "evt_synthetic_retry".parse()?;
    assert!(matches!(
        inbox.admit_synthetic(request(
            &retry_event,
            &session,
            &input,
            "completed",
            &metadata
        ))?,
        AdmitOutcome::Reconciled { .. }
    ));
    let conflict_event: EventId = "evt_synthetic_conflict".parse()?;
    assert!(inbox
        .admit_synthetic(request(
            &conflict_event,
            &session,
            &input,
            "different",
            &metadata
        ))
        .is_err());

    let pending = inbox.pending_inputs(&session)?;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].kind, PromotedInputKind::Synthetic);
    assert_eq!(
        pending[0].description.as_deref(),
        Some("background completion")
    );
    assert_eq!(pending[0].metadata.as_ref(), Some(&metadata));
    let promotion_event: EventId = "evt_synthetic_promote".parse()?;
    let mut mint = || promotion_event.clone();
    let promoted = inbox.promote(
        &session,
        PromoteScope::InputAtIdle,
        &mut mint,
        1_789_560_000_001,
    )?;
    assert_eq!(promoted.len(), 1);
    let history = inbox.promoted_messages(&session, 8)?;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].kind, PromotedInputKind::Synthetic);
    assert_eq!(history[0].text, "completed");
    assert_eq!(history[0].metadata.as_ref(), Some(&metadata));

    let raw: String = inbox.store().connection().query_row(
        "SELECT data FROM event WHERE id = ?1",
        [event.as_str()],
        |row| row.get(0),
    )?;
    let durable: serde_json::Value = serde_json::from_str(&raw)?;
    assert_eq!(durable.pointer("/input/rustAuthorized"), Some(&json!(true)));

    let legacy = InboxEvent::decode(
        "session.input.admitted",
        1,
        &json!({
            "sessionID":session.as_str(),
            "inputID":"msg_legacy_synthetic",
            "input":{"type":"synthetic","delivery":"steer","data":{"text":"legacy"}},
        }),
    )?
    .ok_or("legacy synthetic event")?;
    assert!(matches!(legacy, InboxEvent::LegacySyntheticAdmitted { .. }));
    Ok(())
}
