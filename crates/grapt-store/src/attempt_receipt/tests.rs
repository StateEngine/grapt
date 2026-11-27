mod corruption_and_recovery;
mod future_failure_fact;
mod lifecycle;
mod partial_text;
mod quarantine_and_terminal;
mod send_permit_contradiction;
mod shared_terminal;

use super::*;
use crate::election::ElectionError;
use crate::write::hostile_rows::{self, ForeignAttemptReceipt};
use crate::EventStore;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SESSION: &str = "ses_033602b20001";
const ATTEMPT: &str = "att_033602b20001";
const TERMINAL: &str = "evt_033602b2t001";
const RESERVED_MS: i64 = 1_786_147_200_000;
const PERMIT_MS: i64 = 1_786_147_200_120;
const SETTLED_MS: i64 = 1_786_147_200_900;
const TERMINAL_MS: i64 = 1_786_147_200_800;

/// A FIXTURE ceiling. Not a production value and names none.
fn ceiling(value: u32) -> AttemptRecordCeiling {
    match NonZeroU32::new(value) {
        Some(value) => AttemptRecordCeiling::new(value),
        None => panic!("a non-zero fixture ceiling"),
    }
}

fn wide() -> AttemptRecordCeiling {
    ceiling(4_096)
}

/// A byte budget generous enough that no witness refuses on it by accident.
const WIDE_BUDGET: usize = 1 << 20;

fn session() -> SessionId {
    match SESSION.parse() {
        Ok(session) => session,
        Err(_) => panic!("the fixture session identity parses"),
    }
}

fn attempt_id(value: &str) -> PhysicalAttemptId {
    match PhysicalAttemptId::new(value) {
        Some(id) => id,
        None => panic!("a non-empty fixture attempt id"),
    }
}

fn terminal_of(event: &str) -> TerminalCoordinates {
    match event.parse() {
        Ok(event) => TerminalCoordinates::new(event, TERMINAL_MS),
        Err(_) => panic!("the fixture event identity parses"),
    }
}

fn terminal() -> TerminalCoordinates {
    terminal_of(TERMINAL)
}

fn reserved_receipt(attempt: &str, step: u32) -> DurableAttemptReceipt {
    let bound = wide().0;
    let attempt = match BoundedAttemptId::new(attempt_id(attempt), bound) {
        Ok(attempt) => attempt,
        Err(error) => panic!("the fixture attempt id fits: {error:?}"),
    };
    let session = match BoundedSessionId::new(session(), bound) {
        Ok(session) => session,
        Err(error) => panic!("the fixture session id fits: {error:?}"),
    };
    match (StepNumber::new(step), AttemptOrdinal::new(1)) {
        (Some(step), Some(ordinal)) => DurableAttemptReceipt::reserved(
            attempt,
            session,
            step,
            ordinal,
            ReservedMarker::new(Timestamp::from_millis(RESERVED_MS)),
        ),
        _ => panic!("the fixture step coordinates are valid"),
    }
}

fn elected_store() -> Result<EventStore, Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    store.elect_writer(&AggregateId::Session(session()))?;
    Ok(store)
}

fn token(store: &EventStore) -> Result<FencedWriter, WriteError> {
    store.fencing_token(&AggregateId::Session(session()))
}

fn reserve_with(
    store: &mut EventStore,
    receipt: &DurableAttemptReceipt,
    terminal: &TerminalCoordinates,
    record_ceiling: AttemptRecordCeiling,
) -> Result<AttemptReceiptReserved, WriteError> {
    let writer = token(store)?;
    store.reserve_attempt_receipt(&ReserveInput {
        receipt,
        writer,
        terminal,
        ceiling: record_ceiling,
    })
}

fn reserve(
    store: &mut EventStore,
    receipt: &DurableAttemptReceipt,
) -> Result<AttemptReceiptReserved, WriteError> {
    reserve_with(store, receipt, &terminal(), wide())
}

fn permit(
    store: &mut EventStore,
    attempt: &PhysicalAttemptId,
    at: i64,
) -> Result<MarkerOutcome, WriteError> {
    let writer = token(store)?;
    let tx = store.governed_transaction()?;
    let outcome = EventStore::permit_attempt_send_within(
        &tx,
        attempt,
        writer,
        wide(),
        RequestSentMarker::new(Timestamp::from_millis(at)),
    )?;
    tx.commit()?.discard();
    Ok(outcome)
}

fn settle(
    store: &mut EventStore,
    attempt: &PhysicalAttemptId,
) -> Result<AttemptSettlement, WriteError> {
    let writer = token(store)?;
    let tx = store.governed_transaction()?;
    let outcome = EventStore::settle_attempt_process_loss_within(
        &tx,
        attempt,
        writer,
        wide(),
        SettledMarker::new(Timestamp::from_millis(SETTLED_MS)),
        &mut [],
    )?;
    tx.commit()?.discard();
    Ok(outcome)
}

fn read(
    store: &EventStore,
    attempt: &PhysicalAttemptId,
) -> Result<Option<AttemptReceiptRecord>, WriteError> {
    store.attempt_receipt(attempt, wide())
}

fn columns(store: &EventStore, attempt: &str) -> Result<StoredRow, Box<dyn std::error::Error>> {
    Ok(store
        .connection()
        .query_row(SELECT_ROW, [attempt], map_row)?)
}

fn terminal_rows(store: &EventStore) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type = 'session.step.failed.2'",
        [],
        |r| r.get(0),
    )?)
}

// L4b2-S — atomic settlement from an ALREADY DURABLE ordinary terminal
// -----------------------------------------------------------------------

fn append_l4b2_ended_terminal(
    store: &mut EventStore,
    event: &str,
    created_ms: i64,
    step: u32,
) -> TestResult {
    let id = match event.parse() {
        Ok(id) => id,
        Err(_) => panic!("the fixture event identity parses"),
    };
    let step = match StepNumber::new(step) {
        Some(step) => step,
        None => panic!("the fixture step number is valid"),
    };
    let fact = grapt_event::session_step_ended::StepEndedFact::new(
        step,
        grapt_types::ProviderCompletionDisposition::AssertedComplete,
    );
    let writer = token(store)?;
    let tx = store.governed_transaction()?;
    crate::step_ended::record(&tx, &session(), &id, writer, created_ms, &fact, &mut [])?;
    tx.commit()?.discard();
    Ok(())
}

fn append_failed_terminal(
    store: &mut EventStore,
    event: &str,
    created_ms: i64,
    step: u32,
) -> TestResult {
    let id = match event.parse() {
        Ok(id) => id,
        Err(_) => panic!("the fixture event identity parses"),
    };
    let step = match StepNumber::new(step) {
        Some(step) => step,
        None => panic!("the fixture step number is valid"),
    };
    let fact = grapt_event::session_step_failed::StepFailedFact::new(
        step,
        grapt_types::StepTerminalReason::ProviderFailure {
            class: grapt_types::ProviderFailureClass::Transport,
        },
    )?;
    let writer = token(store)?;
    let tx = store.governed_transaction()?;
    crate::step_failed::record(&tx, &session(), &id, writer, created_ms, &fact, &mut [])?;
    tx.commit()?.discard();
    Ok(())
}

fn ordinary_terminal_rows(store: &EventStore) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(store.connection().query_row(
        "SELECT COUNT(*) FROM event
          WHERE type IN ('session.step.ended.2', 'session.step.failed.2')",
        [],
        |row| row.get(0),
    )?)
}

fn settle_ordinary(
    store: &mut EventStore,
    attempt: &PhysicalAttemptId,
) -> Result<AttemptReceiptReclaimed, WriteError> {
    let writer = token(store)?;
    let tx = store.governed_transaction()?;
    let outcome = EventStore::settle_attempt_ordinary_terminal_within(
        &tx,
        attempt,
        writer,
        wide(),
        SettledMarker::new(Timestamp::from_millis(SETTLED_MS)),
    )?;
    tx.commit()?.discard();
    Ok(outcome)
}

fn stage(
    store: &EventStore,
    row: ForeignAttemptReceipt<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    hostile_rows::install_attempt_receipt(store.connection(), store.gate(), row)?;
    Ok(())
}

fn base_row(bytes: &[u8]) -> ForeignAttemptReceipt<'_> {
    ForeignAttemptReceipt {
        attempt_id: ATTEMPT,
        session_id: SESSION,
        step: 3,
        ordinal: 1,
        writer_epoch: 1,
        terminal_event_id: TERMINAL,
        terminal_created_ms: TERMINAL_MS,
        reserved_ms: RESERVED_MS,
        request_sent_ms: None,
        settled_ms: None,
        record_bytes: bytes,
    }
}
