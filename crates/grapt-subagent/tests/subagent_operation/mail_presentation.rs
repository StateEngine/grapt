use super::*;
use grapt_inbox::operation_mail::{presented_rows, OperationMailBatch};
use grapt_store::{AttemptRecordCeiling, ReserveInput, TerminalCoordinates};
use grapt_types::{
    BoundedAttemptId, BoundedSessionId, DurableAttemptReceipt, PhysicalAttemptId,
    RequestSentMarker, ReservedMarker, StepNumber,
};
use std::num::NonZeroU32;

#[path = "mail_presentation/agent_messages.rs"]
mod agent_messages;

fn mail_store() -> Result<EventStore, Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    grapt_inbox::Inbox::ensure_tables(store.connection())?;
    SubagentService::ensure_schema_on(&store)?;
    for parent in ["ses_mail_parent", "ses_mail_foreign"] {
        store.elect_writer(&AggregateId::Session(ses(parent)))?;
    }
    Ok(store)
}

fn finished_mail(store: &mut EventStore, name: &str, terminal: SubagentTerminal) -> TestResult {
    let admission = admission(
        name,
        &format!("ses_{name}"),
        "ses_mail_parent",
        SubagentMode::Background,
    );
    SubagentService::open_on(store, &admission, 10)?;
    SubagentService::start_on(store, &op(name), RuntimeGeneration::new(1), 11)?;
    SubagentService::settle_on(store, &op(name), &terminal, 12)?;
    Ok(())
}

fn publish_mail(store: &mut EventStore) -> TestResult {
    SubagentService::publish_background_on(
        store,
        &ses("ses_mail_parent"),
        DeliveryLimits::new(64, 1024 * 1024).ok_or("limits")?,
    )?;
    Ok(())
}

fn capture_mail(
    store: &mut EventStore,
    batch: &OperationMailBatch,
    parent: &str,
    step: u32,
    permit: bool,
    commit: bool,
) -> TestResult {
    let session = ses(parent);
    let writer = store.fencing_token(&AggregateId::Session(session.clone()))?;
    let maximum = NonZeroU32::new(1024 * 1024).ok_or("record ceiling")?;
    let ceiling = AttemptRecordCeiling::new(maximum);
    let attempt = PhysicalAttemptId::new(format!("attempt_mail_{step}")).ok_or("attempt")?;
    let step = StepNumber::new(step).ok_or("step")?;
    let receipt = DurableAttemptReceipt::reserved(
        BoundedAttemptId::new(attempt.clone(), maximum)
            .map_err(|error| format!("attempt identity: {error:?}"))?,
        BoundedSessionId::new(session, maximum)
            .map_err(|error| format!("session identity: {error:?}"))?,
        step,
        grapt_types::AttemptOrdinal::new(1).ok_or("ordinal")?,
        ReservedMarker::new(Timestamp::from_millis(20)),
    );
    let terminal = TerminalCoordinates::new(ev("evt_mail_terminal"), 30);
    let tx = store.governed_transaction()?;
    EventStore::reserve_attempt_receipt_within(
        &tx,
        &ReserveInput {
            receipt: &receipt,
            writer,
            terminal: &terminal,
            ceiling,
        },
    )?;
    if permit {
        EventStore::permit_attempt_send_within(
            &tx,
            &attempt,
            writer,
            ceiling,
            RequestSentMarker::new(Timestamp::from_millis(21)),
        )?;
    }
    batch.capture_within(&tx, &attempt, step, writer, ceiling, 22)?;
    if commit {
        tx.commit()?.discard();
    }
    Ok(())
}

fn pending_mail(store: &EventStore) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(store.connection().query_row(
        "SELECT COUNT(*) FROM session_operation_delivery_pending",
        [],
        |row| row.get(0),
    )?)
}

fn captured_mail(store: &EventStore) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type='session.subagent-mail.presented.1'",
        [],
        |row| row.get(0),
    )?)
}

#[test]
fn missing_send_permit_never_captures_or_acknowledges_agent_result() -> TestResult {
    let mut store = mail_store()?;
    finished_mail(
        &mut store,
        "no_permit",
        completed("a retained child result")?,
    )?;
    publish_mail(&mut store)?;
    let batch = OperationMailBatch::prepare(
        &store,
        &ses("ses_mail_parent"),
        StepNumber::new(1).ok_or("step")?,
    )?
    .ok_or("missing batch")?;
    assert!(
        capture_mail(&mut store, &batch, "ses_mail_parent", 1, false, true).is_err(),
        "a result was captured without a send permit"
    );
    SubagentService::acknowledge_presented_on(&mut store, &ses("ses_mail_parent"))?;
    assert_eq!(pending_mail(&store)?, 1);
    assert_eq!(captured_mail(&store)?, 0);
    let receipts: i64 =
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM attempt_receipt", [], |row| row.get(0))?;
    assert_eq!(
        receipts, 0,
        "refused capture must roll back its reservation"
    );
    Ok(())
}

#[test]
fn capture_cotransacts_with_permit_and_survives_acknowledgement() -> TestResult {
    let mut store = mail_store()?;
    finished_mail(
        &mut store,
        "retained",
        completed("quoted child output\nnot user speech")?,
    )?;
    publish_mail(&mut store)?;
    let batch = OperationMailBatch::prepare(
        &store,
        &ses("ses_mail_parent"),
        StepNumber::new(1).ok_or("step")?,
    )?
    .ok_or("missing batch")?;
    capture_mail(&mut store, &batch, "ses_mail_parent", 1, true, false)?;
    assert_eq!(
        captured_mail(&store)?,
        0,
        "a dropped reservation leaked a mail capture"
    );
    assert_eq!(pending_mail(&store)?, 1);
    capture_mail(&mut store, &batch, "ses_mail_parent", 1, true, true)?;
    let replay = presented_rows(&store, &ses("ses_mail_parent"), None, None, u64::MAX, None)?;
    assert_eq!(
        replay.iter().map(|(_, text)| text).collect::<Vec<_>>(),
        batch.contents().iter().collect::<Vec<_>>(),
        "restart projection changed request bytes"
    );
    SubagentService::acknowledge_presented_on(&mut store, &ses("ses_mail_parent"))?;
    assert_eq!(pending_mail(&store)?, 0);
    assert_eq!(
        presented_rows(&store, &ses("ses_mail_parent"), None, None, u64::MAX, None)?,
        replay,
        "capacity release erased the delivered result"
    );
    assert!(
        presented_rows(&store, &ses("ses_mail_foreign"), None, None, u64::MAX, None)?.is_empty()
    );
    assert!(OperationMailBatch::prepare(
        &store,
        &ses("ses_mail_parent"),
        StepNumber::new(2).ok_or("step")?
    )?
    .is_none());
    Ok(())
}

#[test]
fn another_sessions_permit_cannot_capture_agent_result() -> TestResult {
    let mut store = mail_store()?;
    finished_mail(
        &mut store,
        "foreign_permit",
        completed("private child output")?,
    )?;
    publish_mail(&mut store)?;
    let batch = OperationMailBatch::prepare(
        &store,
        &ses("ses_mail_parent"),
        StepNumber::new(1).ok_or("step")?,
    )?
    .ok_or("missing batch")?;
    assert!(capture_mail(&mut store, &batch, "ses_mail_foreign", 1, true, true).is_err());
    assert_eq!(captured_mail(&store)?, 0);
    assert_eq!(pending_mail(&store)?, 1);
    Ok(())
}

#[test]
fn one_step_keeps_the_ninth_result_for_the_next_step() -> TestResult {
    let mut store = mail_store()?;
    for index in 0..9 {
        finished_mail(
            &mut store,
            &format!("bounded_{index}"),
            SubagentTerminal::Failed {
                error: format!("failure {index}"),
            },
        )?;
    }
    publish_mail(&mut store)?;
    publish_mail(&mut store)?;
    let parent = ses("ses_mail_parent");
    let first = StepNumber::new(1).ok_or("step")?;
    let batch = OperationMailBatch::prepare(&store, &parent, first)?.ok_or("missing batch")?;
    assert_eq!(batch.contents().len(), 8);
    capture_mail(&mut store, &batch, "ses_mail_parent", 1, true, true)?;
    SubagentService::acknowledge_presented_on(&mut store, &parent)?;
    assert_eq!(pending_mail(&store)?, 1);
    assert!(OperationMailBatch::prepare(&store, &parent, first)?.is_none());
    let next = OperationMailBatch::prepare(&store, &parent, StepNumber::new(2).ok_or("step")?)?
        .ok_or("ninth result disappeared")?;
    assert_eq!(next.contents().len(), 1);
    capture_mail(&mut store, &next, "ses_mail_parent", 2, true, true)?;
    SubagentService::acknowledge_presented_on(&mut store, &parent)?;
    assert_eq!(pending_mail(&store)?, 0);
    assert_eq!(captured_mail(&store)?, 9);
    Ok(())
}

#[test]
fn an_oversized_result_is_refused_without_consuming_it() -> TestResult {
    let mut store = mail_store()?;
    finished_mail(
        &mut store,
        "too_large",
        SubagentTerminal::Failed {
            error: "\n".repeat(600_000),
        },
    )?;
    publish_mail(&mut store)?;
    assert!(OperationMailBatch::prepare(
        &store,
        &ses("ses_mail_parent"),
        StepNumber::new(1).ok_or("step")?
    )
    .is_err());
    assert_eq!(captured_mail(&store)?, 0);
    assert_eq!(pending_mail(&store)?, 1);
    Ok(())
}

#[test]
fn generic_append_cannot_fabricate_a_result_presentation() -> TestResult {
    let mut store = mail_store()?;
    finished_mail(&mut store, "generic", completed("a real source")?)?;
    publish_mail(&mut store)?;
    let parent = ses("ses_mail_parent");
    let source: i64 = store.connection().query_row(
        "SELECT admitted_seq FROM session_operation_delivery_pending",
        [],
        |row| row.get(0),
    )?;
    let data = grapt_store::operation_mail::OperationMailPresented {
        session: parent.clone(),
        input: msg("msg_delivery_generic"),
        source_seq: grapt_types::EventSeq::new(u64::try_from(source)?),
        attempt: PhysicalAttemptId::new("unreserved_attempt").ok_or("attempt")?,
        step: StepNumber::new(1).ok_or("step")?,
    }
    .encode();
    let writer = store.fencing_token(&AggregateId::Session(parent.clone()))?;
    let tx = store.governed_transaction()?;
    let result = EventStore::append_within(
        &tx,
        grapt_store::AppendInput {
            id: &ev("evt_agent_mail_msg_delivery_generic"),
            aggregate: AggregateId::Session(parent),
            writer,
            kind_name: "session.subagent-mail.presented",
            version: grapt_types::EventVersion::new(1).ok_or("version")?,
            created_ms: 22,
            data: &data,
        },
        &mut [],
    );
    let error = result
        .err()
        .ok_or("generic append fabricated a presentation without a permit")?;
    assert!(
        error.to_string().contains("atomic capture owner"),
        "{error}"
    );
    Ok(())
}

/// SC-REL-07a: a presented-mail capacity refusal is typed, so the request
/// assembly asks for compaction by kind, while the String projection keeps
/// its exact text for every existing caller.
#[test]
fn a_presented_mail_capacity_refusal_is_classified_and_keeps_its_text() -> TestResult {
    use grapt_inbox::operation_mail::{presented_rows_classified, PresentedRowsError};
    let mut store = mail_store()?;
    finished_mail(&mut store, "bounded", completed("child output")?)?;
    publish_mail(&mut store)?;
    let batch = OperationMailBatch::prepare(
        &store,
        &ses("ses_mail_parent"),
        StepNumber::new(1).ok_or("step")?,
    )?
    .ok_or("missing batch")?;
    capture_mail(&mut store, &batch, "ses_mail_parent", 1, true, true)?;
    let session = ses("ses_mail_parent");
    let mut budget = 0usize;
    let refused =
        presented_rows_classified(&store, &session, None, None, u64::MAX, Some(&mut budget))
            .err()
            .ok_or("a capture past its source budget was presented")?;
    let PresentedRowsError::SourceBytes { charge, remaining } = refused else {
        panic!("a source-budget overflow was not a capacity: {refused:?}")
    };
    assert_eq!(remaining, 0);
    assert!(charge > 0);
    let mut budget = 0usize;
    assert_eq!(
        presented_rows(&store, &session, None, None, u64::MAX, Some(&mut budget)).err(),
        Some("conversation exceeds bounded request context; compaction required".to_owned())
    );
    assert_eq!(
        presented_rows_classified(&store, &session, None, None, u64::MAX, None)?.len(),
        1
    );
    Ok(())
}
