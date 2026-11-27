use super::{
    classify_recovery, drive_test_attempt, reserve_permit_handoff, settle_process_loss_recovery,
    test_opener, test_stopped_adapter, PhysicalAttemptOpenInput, PhysicalAttemptOwnerError,
    PhysicalAttemptRecovery, PrivateOpener,
};
use crate::DurableFact;
use grapt_store::{
    AggregateId, AttemptRecordCeiling, EventStore, ReserveInput, TerminalCoordinates, WriteError,
};
use grapt_types::{
    AttemptOrdinal, BoundedAttemptId, BoundedSessionId, DurableAttemptReceipt, IdAuthority,
    IdToken, PhysicalAttemptId, RequestSentMarker, ReservedMarker, SessionId, SettledMarker,
    StepNumber, Timestamp,
};
use std::cell::Cell;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
#[derive(Debug)]
struct CountingIds<'a>(Cell<u32>, &'a str);
impl IdAuthority for CountingIds<'_> {
    fn mint(&self) -> IdToken {
        let next = self.0.get() + 1;
        self.0.set(next);
        let Some(token) = IdToken::new(format!("{}{next}", self.1)) else {
            panic!("formatted test token must be non-empty");
        };
        token
    }
}
fn session(value: &str) -> SessionId {
    let Ok(value) = value.parse() else {
        panic!("test session must parse");
    };
    value
}
fn attempt(value: &str) -> PhysicalAttemptId {
    let Some(value) = PhysicalAttemptId::new(value) else {
        panic!("test attempt must parse");
    };
    value
}
fn ceiling(value: u32) -> AttemptRecordCeiling {
    let Some(value) = NonZeroU32::new(value) else {
        panic!("test ceiling must be non-zero");
    };
    AttemptRecordCeiling::new(value)
}
fn step() -> StepNumber {
    let Some(value) = StepNumber::new(1) else {
        panic!("step one must be valid");
    };
    value
}
fn terminal(value: &str) -> TerminalCoordinates {
    let Ok(event) = value.parse() else {
        panic!("terminal event must parse");
    };
    TerminalCoordinates::new(event, 13)
}
fn store(session: &SessionId) -> EventStore {
    let Ok(mut store) = EventStore::open_in_memory() else {
        panic!("in-memory store must open");
    };
    assert!(store
        .elect_writer(&AggregateId::Session(session.clone()))
        .is_ok());
    store
}
fn input<'a, R>(
    ids: &'a dyn IdAuthority,
    ceiling: AttemptRecordCeiling,
    session: &'a SessionId,
    terminal: &'a TerminalCoordinates,
    reserved: i64,
    permitted: i64,
    opener: PrivateOpener<R>,
) -> PhysicalAttemptOpenInput<'a, R> {
    PhysicalAttemptOpenInput {
        ids,
        ceiling,
        session,
        step: step(),
        ordinal: AttemptOrdinal::FIRST,
        reserved: ReservedMarker::new(Timestamp::from_millis(reserved)),
        send_permit: RequestSentMarker::new(Timestamp::from_millis(permitted)),
        terminal,
        opener,
    }
}
fn reserve(
    store: &mut EventStore,
    attempt: &PhysicalAttemptId,
    session: &SessionId,
    terminal: &TerminalCoordinates,
) {
    let Some(maximum) = NonZeroU32::new(128) else {
        panic!("test maximum must be non-zero");
    };
    let receipt = DurableAttemptReceipt::reserved(
        match BoundedAttemptId::new(attempt.clone(), maximum) {
            Ok(value) => value,
            Err(error) => panic!("test attempt must fit: {error:?}"),
        },
        match BoundedSessionId::new(session.clone(), maximum) {
            Ok(value) => value,
            Err(error) => panic!("test session must fit: {error:?}"),
        },
        step(),
        AttemptOrdinal::FIRST,
        ReservedMarker::new(Timestamp::from_millis(10)),
    );
    let writer = match store.fencing_token(&AggregateId::Session(session.clone())) {
        Ok(value) => value,
        Err(error) => panic!("test aggregate must retain writer: {error}"),
    };
    if let Err(error) = store.reserve_attempt_receipt(&ReserveInput {
        receipt: &receipt,
        writer,
        terminal,
        ceiling: ceiling(128),
    }) {
        panic!("test receipt must reserve: {error}");
    }
}
fn zero_effect_refusal(
    ids: &dyn IdAuthority,
    limit: AttemptRecordCeiling,
    session: &SessionId,
    terminal: &TerminalCoordinates,
    attempt: &PhysicalAttemptId,
) -> Result<(), PhysicalAttemptOwnerError> {
    let mut store = store(session);
    let opens = Arc::new(AtomicUsize::new(0));
    let result = reserve_permit_handoff(
        &mut store,
        input(
            ids,
            limit,
            session,
            terminal,
            10,
            11,
            test_opener(test_stopped_adapter(), Arc::clone(&opens)),
        ),
    );
    assert_eq!(opens.load(Ordering::SeqCst), 0);
    assert!(store
        .attempt_receipt(attempt, ceiling(128))
        .is_ok_and(|receipt| receipt.is_none()));
    result.map(|_| ())
}
#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn supervisor_handoff_reserves_permits_opens_once_and_ends_once() {
    let session = session("ses_owner_success");
    let mut owner_store = store(&session);
    let ids = CountingIds(Cell::new(0), "attempt-");
    let owner_terminal = terminal("evt_owner_success");
    let opens = Arc::new(AtomicUsize::new(0));
    let adapter = test_stopped_adapter();
    let permitted = match reserve_permit_handoff(
        &mut owner_store,
        input(
            &ids,
            ceiling(128),
            &session,
            &owner_terminal,
            10,
            11,
            test_opener(adapter, Arc::clone(&opens)),
        ),
    ) {
        Ok(value) => value,
        Err(error) => panic!("owned handoff must reserve and permit: {error}"),
    };
    let opened = permitted.attempt().clone();
    assert_eq!((ids.0.get(), opens.load(Ordering::SeqCst)), (1, 0));
    let owner = drive_test_attempt(permitted).await;
    assert!(matches!(
        owner.committed.first(),
        Some(DurableFact::AttemptStarted { attempt })
            if attempt == &opened
    ));
    assert!(opens.load(Ordering::SeqCst) == 1 && owner.attempt_terminal_count() == 1);
    let retry = match reserve_permit_handoff(
        &mut owner_store,
        input(
            &ids,
            ceiling(128),
            &session,
            &owner_terminal,
            12,
            13,
            PrivateOpener::new(|_| ()),
        ),
    ) {
        Ok(value) => value,
        Err(error) => panic!("retry physical attempt must permit: {error}"),
    };
    assert!(opened != retry.attempt().clone() && ids.0.get() == 2);
    let mut conflict_store = store(&session);
    reserve(
        &mut conflict_store,
        &attempt("attempt-1"),
        &session,
        &terminal("evt_conflict"),
    );
    let recovery = classify_recovery(&conflict_store, &attempt("attempt-1"), ceiling(128));
    assert!(recovery.is_ok_and(|value| value == PhysicalAttemptRecovery::NoneInitiated));
    let conflict_opens = Arc::new(AtomicUsize::new(0));
    let conflict = reserve_permit_handoff(
        &mut conflict_store,
        input(
            &CountingIds(Cell::new(0), "attempt-"),
            ceiling(128),
            &session,
            &owner_terminal,
            10,
            11,
            test_opener(test_stopped_adapter(), Arc::clone(&conflict_opens)),
        ),
    );
    if let Err(error) = &conflict {
        assert!(matches!(
            error,
            PhysicalAttemptOwnerError::Store(WriteError::ReceiptConflict { .. })
        ));
    }
    if let Ok(permitted) = conflict {
        let _ = drive_test_attempt(permitted).await;
    }
    assert_eq!(conflict_opens.load(Ordering::SeqCst), 0);
    let mut marker_store = store(&session);
    let marker_opens = Arc::new(AtomicUsize::new(0));
    let marker = reserve_permit_handoff(
        &mut marker_store,
        input(
            &CountingIds(Cell::new(0), "attempt-"),
            ceiling(128),
            &session,
            &owner_terminal,
            10,
            9,
            test_opener(test_stopped_adapter(), Arc::clone(&marker_opens)),
        ),
    );
    let marker_opened = match marker {
        Err(PhysicalAttemptOwnerError::Store(WriteError::ReceiptMarkerOrder { .. })) => false,
        Ok(permitted) => {
            let _ = drive_test_attempt(permitted).await;
            true
        }
        Err(error) => panic!("marker refusal must retain its reason: {error}"),
    };
    assert!(!marker_opened && marker_opens.load(Ordering::SeqCst) == 0);
    assert!(marker_store
        .attempt_receipt(&attempt("attempt-1"), ceiling(128))
        .is_ok_and(
            |receipt| receipt.is_some_and(|value| value.receipt().request_sent_at().is_none())
        ));
    let receipt = match owner_store.attempt_receipt(&opened, ceiling(128)) {
        Ok(Some(value)) => value,
        Ok(None) => panic!("permitted attempt must retain its receipt"),
        Err(error) => panic!("receipt must read: {error}"),
    };
    assert!(receipt.receipt().request_sent_at().is_some());
    assert!(matches!(
        classify_recovery(&owner_store, &opened, ceiling(128)),
        Ok(PhysicalAttemptRecovery::AmbiguousProcessLoss)
    ));
    assert!(matches!(
        settle_process_loss_recovery(
            &mut owner_store,
            &opened,
            ceiling(128),
            SettledMarker::new(Timestamp::from_millis(22)),
            &mut [],
        ),
        Ok(grapt_store::AttemptSettlement::Settled)
    ));
    assert!(matches!(
        classify_recovery(&owner_store, &opened, ceiling(128)),
        Ok(PhysicalAttemptRecovery::AlreadySettled)
    ));
}
#[tokio::test]
async fn one_ceiling_refuses_attempt_session_and_whole_record_before_effects() {
    let limit = ceiling(70);
    let short_session = session("ses_short");
    let terminal = terminal("evt_owner_bounds");
    let overlong = "a".repeat(71);
    let long_ids = CountingIds(Cell::new(0), &overlong);
    let denied_attempt = zero_effect_refusal(
        &long_ids,
        limit,
        &short_session,
        &terminal,
        &attempt(&format!("{overlong}1")),
    );
    assert!(matches!(
        denied_attempt,
        Err(PhysicalAttemptOwnerError::AttemptBound(_))
    ));
    let long_session = session(&format!("ses_{}", "s".repeat(71)));
    let session_ids = CountingIds(Cell::new(0), "att_short");
    let denied_session = zero_effect_refusal(
        &session_ids,
        limit,
        &long_session,
        &terminal,
        &attempt("att_short1"),
    );
    assert!(matches!(
        denied_session,
        Err(PhysicalAttemptOwnerError::SessionBound(_))
    ));
    let record_ids = CountingIds(Cell::new(0), "att_record");
    let record_session = session("ses_record");
    let mut record_store = store(&record_session);
    let record_opens = Arc::new(AtomicUsize::new(0));
    let record = reserve_permit_handoff(
        &mut record_store,
        input(
            &record_ids,
            limit,
            &record_session,
            &terminal,
            10,
            11,
            test_opener(test_stopped_adapter(), Arc::clone(&record_opens)),
        ),
    );
    match record {
        Err(PhysicalAttemptOwnerError::Store(WriteError::ReceiptRecordTooLarge { .. })) => {}
        Ok(permitted) => {
            let _owner = drive_test_attempt(permitted).await;
            assert_eq!(
                record_opens.load(Ordering::SeqCst),
                0,
                "record overflow must not open"
            );
        }
        Err(error) => panic!("record ceiling must name its durable refusal: {error}"),
    }
    assert_eq!(record_opens.load(Ordering::SeqCst), 0);
    assert!(matches!(
        record_store.attempt_receipt(&attempt("att_record1"), ceiling(128)),
        Ok(None)
    ));
}
