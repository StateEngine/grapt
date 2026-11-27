use grapt_event::session_tool_terminal::{decode, ToolTerminalFact};
use grapt_store::{AggregateId, Appended, EventStore, WriteError};
use grapt_types::{
    EventId, PhysicalAttemptId, SessionId, ToolCallId, ToolFailureReason, ToolTerminalOutcome,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fact(reason: ToolFailureReason) -> Result<ToolTerminalFact, &'static str> {
    Ok(ToolTerminalFact::new(
        PhysicalAttemptId::new("att_store_tool").ok_or("attempt")?,
        ToolCallId::new("call_store_tool").ok_or("call")?,
        ToolTerminalOutcome::Failed {
            reason,
            detail: None,
        },
    ))
}

#[test]
fn exact_retry_is_idempotent_changed_payload_diverges_and_replay_is_closed() -> TestResult {
    let session: SessionId = "ses_store_tool".parse()?;
    let aggregate = AggregateId::Session(session.clone());
    let event_id: EventId = "evt_store_tool".parse()?;
    let mut store = EventStore::open_in_memory()?;
    store.elect_writer(&aggregate)?;
    let writer = store.fencing_token(&aggregate)?;
    let original = fact(ToolFailureReason::EffectFailed)?;

    for expected in [
        Appended::Committed { seq: 0 },
        Appended::Idempotent { seq: 0 },
    ] {
        let tx = store.governed_transaction()?;
        let appended = grapt_store::tool_terminal::record(
            &tx,
            &session,
            &event_id,
            writer,
            41,
            &original,
            &mut [],
        )?;
        tx.commit()?.discard();
        assert_eq!(appended, expected);
    }

    let changed = fact(ToolFailureReason::CaptureFailed)?;
    let tx = store.governed_transaction()?;
    let refusal =
        grapt_store::tool_terminal::record(&tx, &session, &event_id, writer, 41, &changed, &mut []);
    assert!(matches!(refusal, Err(WriteError::Divergence { .. })));
    drop(tx);

    let data: String = store.connection().query_row(
        "SELECT data FROM event WHERE id = ?1",
        [event_id.as_str()],
        |row| row.get(0),
    )?;
    let value: serde_json::Value = serde_json::from_str(&data)?;
    assert_eq!(decode(&value)?, original);
    Ok(())
}
