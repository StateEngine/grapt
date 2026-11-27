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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcessLossRecoveryReport {
    pub sessions: usize,
    pub settled: usize,
    pub reclaimed: usize,
    pub quarantined: usize,
    pub bytes_read: usize,
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
            return Err(bound(report.sessions, rows, report.bytes_read));
        }
        store.elect_writer(&AggregateId::Session(session.clone()))?;
        report.sessions = report.sessions.saturating_add(1);

        let mut after_attempt: Option<PhysicalAttemptId> = None;
        loop {
            let remaining = if rows == MAX_RECOVERY_ROWS {
                0
            } else {
                MAX_RECOVERY_BYTES.saturating_sub(report.bytes_read)
            };
            match store.next_attempt_recovery_row(
                &session,
                after_attempt.as_ref(),
                remaining,
                ceiling,
            )? {
                RecoveryStep::Exhausted => break,
                RecoveryStep::BudgetExhausted {
                    declared_bytes,
                    remaining,
                } => {
                    if rows == MAX_RECOVERY_ROWS {
                        return Err(bound(report.sessions, rows, report.bytes_read));
                    }
                    return Err(ProcessLossRecoveryError::ByteBudget {
                        declared: declared_bytes,
                        remaining,
                    });
                }
                RecoveryStep::Row {
                    row,
                    next_after,
                    bytes_charged,
                } => {
                    if rows == MAX_RECOVERY_ROWS {
                        return Err(bound(report.sessions, rows, report.bytes_read));
                    }
                    report.bytes_read = report
                        .bytes_read
                        .checked_add(bytes_charged)
                        .ok_or_else(|| bound(report.sessions, rows, report.bytes_read))?;
                    reconcile_row(store, clock, ceiling, &session, row, &mut report)?;
                    rows = rows.saturating_add(1);
                    after_attempt = Some(next_after);
                }
            }
        }
        after_session = Some(session);
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
            .map_err(|error| ProcessLossRecoveryError::Attempt(error.to_string()))?;
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
}
