//! Bounded process-start recovery for durable Physical Attempts.
//!
//! A send-permitted receipt without a terminal is ambiguous after process
//! loss. Startup settles it to the exact preallocated `ProcessLoss` terminal
//! and never re-enters a provider boundary. Settled receipts are reclaimed
//! only after the store proves their terminal; unreadable receipts move to the
//! durable quarantine before the cursor advances.

use grapt_event::AggregateId;
use grapt_store::{
    AttemptRecordCeiling, AttemptSettlement, EventStore, RecoveryRow, RecoveryStep, WriteError,
};
use grapt_types::{ClockAuthority, PhysicalAttemptId, SessionId, SettledMarker};

use crate::physical_attempt_owner::settle_process_loss_recovery;

pub const MAX_RECOVERY_SESSIONS: usize = 64;
pub const MAX_RECOVERY_ROWS: usize = 128;
pub const MAX_RECOVERY_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessLossRecoveryReport {
    pub sessions: usize,
    pub settled: usize,
    pub reclaimed: usize,
    pub quarantined: usize,
    pub bytes_read: usize,
    /// Items this pass skipped, each with its reason. A skipped receipt stays
    /// durable and unsettled, so nothing is replayed and the next start sees
    /// it again; it never stops the server from starting.
    pub warnings: Vec<RecoveryWarning>,
}

/// One item a recovery pass could not settle, or a bound that ended the pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryWarning {
    pub session: Option<SessionId>,
    pub attempt: Option<PhysicalAttemptId>,
    pub reason: String,
}

impl std::fmt::Display for RecoveryWarning {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(session) = &self.session {
            write!(formatter, "session {session} ")?;
        }
        if let Some(attempt) = &self.attempt {
            write!(formatter, "attempt {} ", attempt.as_str())?;
        }
        write!(formatter, "{}", self.reason)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProcessLossRecoveryError {
    #[error(transparent)]
    Store(#[from] WriteError),
    #[error("attempt recovery refused: {0}")]
    Attempt(String),
    #[error(
        "attempt recovery exceeded its bounded pass ({sessions} sessions, {rows} rows, {bytes} bytes)"
    )]
    Bound {
        sessions: usize,
        rows: usize,
        bytes: usize,
    },
    #[error(
        "attempt recovery next row declares {declared} bytes with only {remaining} bytes left"
    )]
    ByteBudget { declared: usize, remaining: usize },
}

/// Reconcile every receipt that fits one bounded startup pass.
///
/// This function has no provider, opener, or retry handle in its signature.
/// Consequently the process-loss path cannot duplicate the external call it
/// is conservatively settling.
///
/// A receipt or Session that cannot be settled is skipped with a warning in
/// the report and left durable for the next start; a bound ends the pass the
/// same way. The database itself failing (read-only, missing schema,
/// unreadable) is an error, as is a failure to enumerate Sessions at all.
pub fn recover_process_loss_attempts(
    store: &mut EventStore,
    clock: &dyn ClockAuthority,
    ceiling: AttemptRecordCeiling,
) -> Result<ProcessLossRecoveryReport, ProcessLossRecoveryError> {
    let mut report = ProcessLossRecoveryReport::default();
    let mut after_session: Option<SessionId> = None;
    let mut rows = 0usize;

    loop {
        let Some(session) = store.next_attempt_recovery_session(after_session.as_ref())? else {
            return Ok(report);
        };
        if report.sessions == MAX_RECOVERY_SESSIONS {
            let reason = bound(report.sessions, rows, report.bytes_read).to_string();
            report.warn(None, None, reason);
            return Ok(report);
        }
        report.sessions = report.sessions.saturating_add(1);
        if let Err(error) = store.elect_writer(&AggregateId::Session(session.clone())) {
            if error.is_storage_failure() {
                return Err(error.into());
            }
            report.warn(
                Some(&session),
                None,
                format!("writer election failed: {error}"),
            );
            after_session = Some(session);
            continue;
        }
        if recover_session(store, clock, ceiling, &session, &mut rows, &mut report)? == Pass::Ended
        {
            return Ok(report);
        }
        after_session = Some(session);
    }
}

impl ProcessLossRecoveryError {
    fn is_storage_failure(&self) -> bool {
        matches!(self, Self::Store(error) if error.is_storage_failure())
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Pass {
    Continue,
    Ended,
}

impl ProcessLossRecoveryReport {
    fn warn(
        &mut self,
        session: Option<&SessionId>,
        attempt: Option<&PhysicalAttemptId>,
        reason: String,
    ) {
        self.warnings.push(RecoveryWarning {
            session: session.cloned(),
            attempt: attempt.cloned(),
            reason,
        });
    }
}

/// One Session's receipts. Returns [`Pass::Ended`] when a bound ends the
/// whole pass, and an error only when the database itself failed.
fn recover_session(
    store: &mut EventStore,
    clock: &dyn ClockAuthority,
    ceiling: AttemptRecordCeiling,
    session: &SessionId,
    rows: &mut usize,
    report: &mut ProcessLossRecoveryReport,
) -> Result<Pass, ProcessLossRecoveryError> {
    let mut after_attempt: Option<PhysicalAttemptId> = None;
    loop {
        let remaining = if *rows == MAX_RECOVERY_ROWS {
            0
        } else {
            MAX_RECOVERY_BYTES.saturating_sub(report.bytes_read)
        };
        let step = match store.next_attempt_recovery_row(
            session,
            after_attempt.as_ref(),
            remaining,
            ceiling,
        ) {
            Ok(step) => step,
            Err(error) if error.is_storage_failure() => return Err(error.into()),
            Err(error) => {
                report.warn(
                    Some(session),
                    after_attempt.as_ref(),
                    format!("receipt unreadable: {error}"),
                );
                return Ok(Pass::Continue);
            }
        };
        match step {
            RecoveryStep::Exhausted => return Ok(Pass::Continue),
            RecoveryStep::BudgetExhausted {
                declared_bytes,
                remaining,
            } => {
                let error = if *rows == MAX_RECOVERY_ROWS {
                    bound(report.sessions, *rows, report.bytes_read)
                } else {
                    ProcessLossRecoveryError::ByteBudget {
                        declared: declared_bytes,
                        remaining,
                    }
                };
                report.warn(Some(session), None, error.to_string());
                return Ok(Pass::Ended);
            }
            RecoveryStep::Row {
                row,
                next_after,
                bytes_charged,
            } => {
                if *rows == MAX_RECOVERY_ROWS {
                    let reason = bound(report.sessions, *rows, report.bytes_read).to_string();
                    report.warn(Some(session), None, reason);
                    return Ok(Pass::Ended);
                }
                report.bytes_read = report.bytes_read.saturating_add(bytes_charged);
                match reconcile_row(store, clock, ceiling, session, row, report) {
                    Err(error) if error.is_storage_failure() => return Err(error),
                    Err(error) => report.warn(Some(session), Some(&next_after), error.to_string()),
                    Ok(()) => {}
                }
                *rows = rows.saturating_add(1);
                after_attempt = Some(next_after);
            }
        }
    }
}

fn reconcile_row(
    store: &mut EventStore,
    clock: &dyn ClockAuthority,
    ceiling: AttemptRecordCeiling,
    session: &SessionId,
    row: RecoveryRow,
    report: &mut ProcessLossRecoveryReport,
) -> Result<(), ProcessLossRecoveryError> {
    match row {
        RecoveryRow::Receipt(record) => {
            let attempt = record.receipt().attempt().clone();
            let mut projectors = [];
            let settlement = settle_process_loss_recovery(
                store,
                &attempt,
                ceiling,
                SettledMarker::new(clock.now()),
                &mut projectors,
            )
            .map_err(|error| match error {
                crate::physical_attempt_owner::PhysicalAttemptOwnerError::Store(write)
                    if write.is_storage_failure() =>
                {
                    ProcessLossRecoveryError::Store(write)
                }
                other => ProcessLossRecoveryError::Attempt(other.to_string()),
            })?;
            if settlement == AttemptSettlement::Settled {
                report.settled = report.settled.saturating_add(1);
            }
            let writer =
                store.fencing_token(&AggregateId::Session(record.receipt().session().clone()))?;
            let transaction = store.governed_transaction().map_err(WriteError::from)?;
            EventStore::reclaim_attempt_receipt_within(&transaction, &attempt, writer, ceiling)?;
            transaction.commit().map_err(WriteError::from)?.discard();
            report.reclaimed = report.reclaimed.saturating_add(1);
        }
        RecoveryRow::Untrustworthy {
            attempt,
            declared_bytes: _,
            reason,
        } => {
            let writer = store.fencing_token(&AggregateId::Session(session.clone()))?;
            let transaction = store.governed_transaction().map_err(WriteError::from)?;
            EventStore::quarantine_attempt_receipt_within(
                &transaction,
                &attempt,
                writer,
                reason,
                clock.now(),
            )?;
            transaction.commit().map_err(WriteError::from)?.discard();
            report.quarantined = report.quarantined.saturating_add(1);
        }
    }
    Ok(())
}

fn bound(sessions: usize, rows: usize, bytes: usize) -> ProcessLossRecoveryError {
    ProcessLossRecoveryError::Bound {
        sessions,
        rows,
        bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_store::{ReserveInput, TerminalCoordinates};
    use grapt_types::{
        AttemptOrdinal, BoundedAttemptId, BoundedSessionId, DurableAttemptReceipt,
        RequestSentMarker, ReservedMarker, StepNumber, Timestamp,
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
    fn sent_attempt_restart_settles_without_replay_and_resurrection_cannot_duplicate() -> TestResult
    {
        let session = session("ses_d1_recovery");
        let attempt = attempt("attempt-d1-recovery");
        let mut store = EventStore::open_in_memory()?;
        stage_sent_attempt(&mut store, &session, &attempt, "evt_d1_recovery")?;

        let first = match recover_process_loss_attempts(&mut store, &FixedClock(40), ceiling(4_096)) {
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

        let second =
            match recover_process_loss_attempts(&mut store, &FixedClock(50), ceiling(4_096)) {
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
    fn row_outside_the_injected_ceiling_is_quarantined_without_a_terminal() -> TestResult {
        let session = session("ses_d1_quarantine");
        let attempt = attempt("attempt-d1-quarantine");
        let mut store = EventStore::open_in_memory()?;
        stage_sent_attempt(&mut store, &session, &attempt, "evt_d1_quarantine")?;

        let report = recover_process_loss_attempts(&mut store, &FixedClock(40), ceiling(1))?;
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

        let report = recover_process_loss_attempts(&mut store, &FixedClock(40), ceiling(4_096))?;
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
    fn a_store_missing_its_writer_schema_fails_recovery_instead_of_settling_nothing() -> TestResult
    {
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
        let result = recover_process_loss_attempts(&mut store, &FixedClock(40), ceiling(4_096));
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
        let report = recover_process_loss_attempts(&mut store, &FixedClock(40), ceiling(4_096))?;
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
}
