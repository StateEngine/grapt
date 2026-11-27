use super::*;
use grapt_store::{ReserveInput, TerminalCoordinates};
use grapt_types::{
    AttemptOrdinal, BoundedAttemptId, BoundedSessionId, DurableAttemptReceipt, RequestSentMarker,
    ReservedMarker, StepNumber, Timestamp,
};
use std::num::NonZeroU32;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug)]
struct FixedClock(i64);

impl ClockAuthority for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

fn session(raw: &str) -> SessionId {
    match raw.parse() {
        Ok(value) => value,
        Err(_) => panic!("fixture Session identity must parse"),
    }
}

fn attempt(raw: &str) -> PhysicalAttemptId {
    match PhysicalAttemptId::new(raw) {
        Some(value) => value,
        None => panic!("fixture Physical Attempt identity must be non-empty"),
    }
}

fn none() -> impl FnMut(&mut EventStore, &SessionId) -> Result<(), String> {
    |_, _| Ok(())
}

fn ceiling(raw: u32) -> AttemptRecordCeiling {
    match NonZeroU32::new(raw) {
        Some(value) => AttemptRecordCeiling::new(value),
        None => panic!("fixture receipt ceiling must be non-zero"),
    }
}

fn stage_sent_attempt(
    store: &mut EventStore,
    session: &SessionId,
    attempt: &PhysicalAttemptId,
    terminal: &str,
) -> TestResult {
    let aggregate = AggregateId::Session(session.clone());
    store.elect_writer(&aggregate)?;
    let bound = match NonZeroU32::new(4_096) {
        Some(value) => value,
        None => panic!("fixture identity bound must be non-zero"),
    };
    let step = match StepNumber::new(1) {
        Some(value) => value,
        None => panic!("fixture Step must be positive"),
    };
    let bounded_attempt = match BoundedAttemptId::new(attempt.clone(), bound) {
        Ok(value) => value,
        Err(error) => panic!("fixture attempt must fit: {error:?}"),
    };
    let bounded_session = match BoundedSessionId::new(session.clone(), bound) {
        Ok(value) => value,
        Err(error) => panic!("fixture Session must fit: {error:?}"),
    };
    let receipt = DurableAttemptReceipt::reserved(
        bounded_attempt,
        bounded_session,
        step,
        AttemptOrdinal::FIRST,
        ReservedMarker::new(Timestamp::from_millis(10)),
    );
    let terminal = TerminalCoordinates::new(terminal.parse()?, 30);
    let writer = store.fencing_token(&aggregate)?;
    store.reserve_attempt_receipt(&ReserveInput {
        receipt: &receipt,
        writer,
        terminal: &terminal,
        ceiling: ceiling(4_096),
    })?;
    let transaction = store.governed_transaction()?;
    EventStore::permit_attempt_send_within(
        &transaction,
        attempt,
        writer,
        ceiling(4_096),
        RequestSentMarker::new(Timestamp::from_millis(20)),
    )?;
    transaction.commit()?.discard();
    Ok(())
}

#[test]
fn sent_attempt_restart_settles_without_replay_and_resurrection_cannot_duplicate() -> TestResult {
    let session = session("ses_d1_recovery");
    let attempt = attempt("attempt-d1-recovery");
    let mut store = EventStore::open_in_memory()?;
    stage_sent_attempt(&mut store, &session, &attempt, "evt_d1_recovery")?;

    let first = match recover_process_loss_attempts(&mut store, &FixedClock(40), ceiling(4_096), &mut none()) {
        Ok(report) => report,
        Err(error) => panic!(
            "request-sent restart must settle the exact ProcessLoss terminal before reclaim: {error}"
        ),
    };
    assert_eq!((first.sessions, first.settled, first.reclaimed), (1, 1, 1));
    assert!(store.attempt_receipt(&attempt, ceiling(4_096))?.is_none());
    let terminal_count: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event
         WHERE aggregate_family = 'session' AND aggregate_id = ?1
           AND type = 'session.step.failed.2'",
        [session.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(
        terminal_count, 1,
        "restart must append exactly one terminal"
    );
    let terminal: String = store.connection().query_row(
        "SELECT data FROM event WHERE id = 'evt_d1_recovery'",
        [],
        |row| row.get(0),
    )?;
    assert!(
        terminal.contains("ambiguous"),
        "the request-sent receipt must retain ambiguous external-effect evidence"
    );

    let second = match recover_process_loss_attempts(
        &mut store,
        &FixedClock(50),
        ceiling(4_096),
        &mut none(),
    ) {
        Ok(report) => report,
        Err(error) => panic!(
            "second restart must observe the reclaimed attempt without resurrecting it: {error}"
        ),
    };
    assert_eq!(second, ProcessLossRecoveryReport::default());
    let resurrected_count: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE id = 'evt_d1_recovery'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        resurrected_count, 1,
        "a second restart resurrected the exact terminal"
    );
    Ok(())
}

#[test]
fn every_receipt_settles_however_many_sessions_and_rows_a_crash_left() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let narrow = RECOVERY_PAGE_SESSIONS + 3;
    for index in 0..narrow {
        let id = format!("{index:03}");
        stage_sent_attempt(
            &mut store,
            &session(&format!("ses_page_{id}")),
            &attempt(&format!("attempt-page-{id}")),
            &format!("evt_page_{id}"),
        )?;
    }
    let wide = session("ses_page_wide");
    let deep = RECOVERY_PAGE_ROWS + 3;
    for index in 0..deep {
        let id = format!("{index:03}");
        stage_sent_attempt(
            &mut store,
            &wide,
            &attempt(&format!("attempt-wide-{id}")),
            &format!("evt_wide_{id}"),
        )?;
    }
    let report =
        recover_process_loss_attempts(&mut store, &FixedClock(40), ceiling(4_096), &mut none())?;
    let rows = narrow + deep;
    assert_eq!(
        (report.sessions, report.settled, report.reclaimed),
        (narrow + 1, rows, rows),
        "{report:?}"
    );
    assert!(report.pages > 2 && report.warnings.is_empty(), "{report:?}");
    let open: i64 =
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM attempt_receipt", [], |row| row.get(0))?;
    assert_eq!(open, 0, "a receipt past the first page was left open");
    Ok(())
}

#[test]
fn row_outside_the_injected_ceiling_is_quarantined_without_a_terminal() -> TestResult {
    let session = session("ses_d1_quarantine");
    let attempt = attempt("attempt-d1-quarantine");
    let mut store = EventStore::open_in_memory()?;
    stage_sent_attempt(&mut store, &session, &attempt, "evt_d1_quarantine")?;

    let report =
        recover_process_loss_attempts(&mut store, &FixedClock(40), ceiling(1), &mut none())?;
    assert_eq!((report.quarantined, report.settled), (1, 0));
    let live: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM attempt_receipt WHERE attempt_id = ?1",
        [attempt.as_str()],
        |row| row.get(0),
    )?;
    let quarantined: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM attempt_receipt_quarantine WHERE attempt_id = ?1",
        [attempt.as_str()],
        |row| row.get(0),
    )?;
    let terminals: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE id = 'evt_d1_quarantine'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!((live, quarantined, terminals), (0, 1, 0));
    Ok(())
}

#[test]
fn an_unsettleable_receipt_is_skipped_and_other_sessions_still_recover() -> TestResult {
    let first = session("ses_d1_a_first");
    let broken = session("ses_d1_b_broken");
    let healthy = session("ses_d1_c_healthy");
    let mut store = EventStore::open_in_memory()?;
    stage_sent_attempt(
        &mut store,
        &first,
        &attempt("attempt-d1-first"),
        "evt_d1_shared",
    )?;
    // Claims a terminal id the first Session's settlement will already own.
    stage_sent_attempt(
        &mut store,
        &broken,
        &attempt("attempt-d1-broken"),
        "evt_d1_shared",
    )?;
    stage_sent_attempt(
        &mut store,
        &healthy,
        &attempt("attempt-d1-healthy"),
        "evt_d1_healthy",
    )?;

    let report =
        recover_process_loss_attempts(&mut store, &FixedClock(40), ceiling(4_096), &mut none())?;
    assert_eq!(report.sessions, 3);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.session.as_ref() == Some(&broken)),
        "the broken receipt must surface as a warning: {report:?}"
    );
    let healthy_terminal: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE id = 'evt_d1_healthy'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(healthy_terminal, 1, "the healthy Session must still settle");
    Ok(())
}

#[test]
fn a_store_missing_its_writer_schema_fails_recovery_instead_of_settling_nothing() -> TestResult {
    let path = std::env::temp_dir().join(format!(
        "grapt-recovery-schema-{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let mut store = EventStore::open(&path, "recovery-schema")?;
    stage_sent_attempt(
        &mut store,
        &session("ses_d1_schema"),
        &attempt("attempt-d1-schema"),
        "evt_d1_schema",
    )?;
    rusqlite::Connection::open(&path)?.execute_batch("DROP TABLE aggregate_writer")?;
    let result =
        recover_process_loss_attempts(&mut store, &FixedClock(40), ceiling(4_096), &mut none());
    assert!(
        matches!(result, Err(ProcessLossRecoveryError::Store(_))),
        "a database failure was reported as success: {result:?}"
    );
    drop(store);
    std::fs::remove_file(&path)?;
    Ok(())
}

#[test]
fn a_mistyped_writer_row_is_skipped_and_other_sessions_still_recover() -> TestResult {
    let path = std::env::temp_dir().join(format!(
        "grapt-recovery-writer-row-{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let mut store = EventStore::open(&path, "recovery-writer-row")?;
    let broken = session("ses_d1_writer_broken");
    for (session, attempt, terminal) in [
        (&broken, "attempt-d1-writer-broken", "evt_d1_writer_broken"),
        (
            &session("ses_d1_writer_healthy"),
            "attempt-d1-writer-healthy",
            "evt_d1_writer_healthy",
        ),
    ] {
        stage_sent_attempt(&mut store, session, &self::attempt(attempt), terminal)?;
    }
    rusqlite::Connection::open(&path)?.execute(
        "UPDATE aggregate_writer SET owner_id = X'00FF' WHERE aggregate_id = ?1",
        [broken.as_str()],
    )?;
    let report =
        recover_process_loss_attempts(&mut store, &FixedClock(40), ceiling(4_096), &mut none())?;
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.session.as_ref() == Some(&broken)),
        "the mistyped writer row must surface as a warning: {report:?}"
    );
    let healthy: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE id = 'evt_d1_writer_healthy'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(healthy, 1, "the healthy Session must still settle");
    drop(store);
    std::fs::remove_file(&path)?;
    Ok(())
}

#[test]
fn a_session_whose_first_step_fails_keeps_its_receipts_for_the_next_start() -> TestResult {
    let failing = session("ses_d1_first_failing");
    let healthy = session("ses_d1_first_healthy");
    let (lost, kept) = (attempt("attempt-d1-lost"), attempt("attempt-d1-kept"));
    let mut store = EventStore::open_in_memory()?;
    stage_sent_attempt(&mut store, &failing, &lost, "evt_d1_first_failing")?;
    stage_sent_attempt(&mut store, &healthy, &kept, "evt_d1_first_healthy")?;
    let mut unmarked = Vec::new();
    let mut first = |store: &mut EventStore, session: &SessionId| {
        let mine = if *session == failing { &lost } else { &kept };
        if !matches!(store.attempt_receipt(mine, ceiling(4_096)), Ok(Some(_))) {
            unmarked.push(session.clone());
        }
        match *session == failing {
            true => Err("lost runs could not be settled".to_owned()),
            false => Ok(()),
        }
    };
    let report =
        recover_process_loss_attempts(&mut store, &FixedClock(40), ceiling(4_096), &mut first)?;
    assert!(
        unmarked.is_empty(),
        "a receipt was retired before its Session's first step"
    );
    assert_eq!((report.settled, report.reclaimed), (1, 1), "{report:?}");
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.session.as_ref() == Some(&failing)),
        "{report:?}"
    );
    assert!(store.attempt_receipt(&lost, ceiling(4_096))?.is_some());
    let next =
        recover_process_loss_attempts(&mut store, &FixedClock(50), ceiling(4_096), &mut none())?;
    assert_eq!(
        (next.sessions, next.settled, next.reclaimed),
        (1, 1, 1),
        "{next:?}"
    );
    assert!(store.attempt_receipt(&lost, ceiling(4_096))?.is_none());
    Ok(())
}

#[test]
fn a_receipt_larger_than_a_page_is_handled_and_its_session_continues() -> TestResult {
    let path = std::env::temp_dir().join(format!(
        "grapt-recovery-large-row-{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let mut store = EventStore::open(&path, "recovery-large-row")?;
    let session = session("ses_d1_large_row");
    for index in 0..3 {
        stage_sent_attempt(
            &mut store,
            &session,
            &attempt(&format!("attempt-d1-large-{index}")),
            &format!("evt_d1_large_{index}"),
        )?;
    }
    rusqlite::Connection::open(&path)?.execute(
        "UPDATE attempt_receipt SET record_bytes = zeroblob(?1) WHERE attempt_id = ?2",
        rusqlite::params![
            i64::try_from(RECOVERY_PAGE_BYTES + 1024)?,
            "attempt-d1-large-0"
        ],
    )?;
    let wide = ceiling(u32::try_from(RECOVERY_PAGE_BYTES * 2)?);
    let report = recover_process_loss_attempts(&mut store, &FixedClock(40), wide, &mut none())?;
    assert_eq!((report.quarantined, report.settled), (1, 2), "{report:?}");
    let open: i64 =
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM attempt_receipt", [], |row| row.get(0))?;
    assert_eq!(open, 0, "a receipt after the large one was left open");
    drop(store);
    std::fs::remove_file(&path)?;
    Ok(())
}
