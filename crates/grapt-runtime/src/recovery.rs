//! Paged process-start recovery for durable Physical Attempts.
//!
//! A send-permitted receipt without a terminal is ambiguous after process
//! loss. Startup settles it to the exact preallocated `ProcessLoss` terminal
//! and never re-enters a provider boundary. Settled receipts are reclaimed
//! only after the store proves their terminal; unreadable receipts move to the
//! durable quarantine before the cursor advances.
//!
//! The pass reads in pages, each bounded in Sessions, rows and bytes, and
//! starts the next page where the last stopped, until every receipt has been
//! visited once. Each receipt and Session is visited at most once, so the pass
//! ends however many a crash left behind, and it holds no more than one page.
//! A page can always hold one receipt the caller's ceiling admits.
//!
//! A Session's receipts are its durable mark that a crash interrupted it, so
//! the caller's per-Session work runs before any of them is retired; if that
//! work fails, the Session's receipts stay for the next start to find again.

use grapt_event::AggregateId;
use grapt_store::{
    AttemptRecordCeiling, AttemptSettlement, EventStore, RecoveryRow, RecoveryStep, WriteError,
};
use grapt_types::{ClockAuthority, PhysicalAttemptId, SessionId, SettledMarker};

use crate::physical_attempt_owner::settle_process_loss_recovery;

/// Sessions one page visits.
pub const RECOVERY_PAGE_SESSIONS: usize = 64;
/// Receipt rows one page reads.
pub const RECOVERY_PAGE_ROWS: usize = 128;
/// Receipt bytes one page materializes, or the caller's per-receipt ceiling
/// when that is larger.
pub const RECOVERY_PAGE_BYTES: usize = 256 * 1024;
/// Warnings kept in full; later ones are only counted.
pub const MAX_RECOVERY_WARNINGS: usize = 64;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessLossRecoveryReport {
    pub sessions: usize,
    pub settled: usize,
    pub reclaimed: usize,
    pub quarantined: usize,
    pub bytes_read: usize,
    /// Pages the pass read; zero when no Session had a receipt.
    pub pages: usize,
    /// Items this pass skipped, each with its reason, up to
    /// [`MAX_RECOVERY_WARNINGS`]. A skipped receipt stays durable and
    /// unsettled, so nothing is replayed and the next start sees it again; it
    /// never stops the server from starting.
    pub warnings: Vec<RecoveryWarning>,
    /// Warnings past the kept ones.
    pub warnings_omitted: usize,
}

/// One item a recovery pass could not settle.
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
        "attempt recovery row declares {declared} bytes, more than an empty page's {remaining}, \
         though the store admits no receipt over that"
    )]
    ByteBudget { declared: usize, remaining: usize },
}

/// Reconcile every receipt, page by page.
///
/// This function has no provider, opener, or retry handle in its signature.
/// Consequently the process-loss path cannot duplicate the external call it
/// is conservatively settling.
///
/// `first` runs once for each Session with a receipt, holding its writer,
/// before any of its receipts is settled or retired. When it fails, the
/// Session is skipped with a warning and its receipts are left for the next
/// start.
///
/// A receipt or Session that cannot be settled is skipped with a warning in
/// the report and left durable for the next start. The database itself
/// failing (read-only, missing schema, unreadable) is an error, as is a
/// failure to enumerate Sessions at all.
pub fn recover_process_loss_attempts(
    store: &mut EventStore,
    clock: &dyn ClockAuthority,
    ceiling: AttemptRecordCeiling,
    first: &mut dyn FnMut(&mut EventStore, &SessionId) -> Result<(), String>,
) -> Result<ProcessLossRecoveryReport, ProcessLossRecoveryError> {
    let mut report = ProcessLossRecoveryReport::default();
    let mut page = Page::default();
    let mut after_session: Option<SessionId> = None;

    loop {
        let Some(session) = store.next_attempt_recovery_session(after_session.as_ref())? else {
            return Ok(report);
        };
        if report.pages == 0 {
            report.pages = 1;
        } else if page.sessions == RECOVERY_PAGE_SESSIONS {
            page.next(&mut report);
        }
        page.sessions = page.sessions.saturating_add(1);
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
        if let Err(reason) = first(store, &session) {
            report.warn(
                Some(&session),
                None,
                format!("receipts kept for the next start: {reason}"),
            );
            after_session = Some(session);
            continue;
        }
        recover_session(store, clock, ceiling, &session, &mut page, &mut report)?;
        after_session = Some(session);
    }
}

impl ProcessLossRecoveryError {
    fn is_storage_failure(&self) -> bool {
        matches!(self, Self::Store(error) if error.is_storage_failure())
    }
}

/// What the current page has spent.
#[derive(Debug, Default)]
struct Page {
    sessions: usize,
    rows: usize,
    bytes: usize,
}

impl Page {
    /// Start a new page that already holds the Session being read.
    fn next(&mut self, report: &mut ProcessLossRecoveryReport) {
        *self = Self {
            sessions: 1,
            rows: 0,
            bytes: 0,
        };
        report.pages = report.pages.saturating_add(1);
    }

    fn fresh(&self) -> bool {
        self.rows == 0 && self.bytes == 0
    }
}

impl ProcessLossRecoveryReport {
    fn warn(
        &mut self,
        session: Option<&SessionId>,
        attempt: Option<&PhysicalAttemptId>,
        reason: String,
    ) {
        if self.warnings.len() < MAX_RECOVERY_WARNINGS {
            self.warnings.push(RecoveryWarning {
                session: session.cloned(),
                attempt: attempt.cloned(),
                reason,
            });
        } else {
            self.warnings_omitted = self.warnings_omitted.saturating_add(1);
        }
    }
}

/// One Session's receipts, across as many pages as they need. Errors only
/// when the database itself failed.
fn recover_session(
    store: &mut EventStore,
    clock: &dyn ClockAuthority,
    ceiling: AttemptRecordCeiling,
    session: &SessionId,
    page: &mut Page,
    report: &mut ProcessLossRecoveryReport,
) -> Result<(), ProcessLossRecoveryError> {
    let mut after_attempt: Option<PhysicalAttemptId> = None;
    let budget = RECOVERY_PAGE_BYTES.max(usize::try_from(ceiling.get()).unwrap_or(usize::MAX));
    loop {
        if page.rows == RECOVERY_PAGE_ROWS {
            page.next(report);
        }
        let step = match store.next_attempt_recovery_row(
            session,
            after_attempt.as_ref(),
            budget.saturating_sub(page.bytes),
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
                break;
            }
        };
        match step {
            RecoveryStep::Exhausted => break,
            // The row was not fetched and the cursor did not move: a new page
            // holds it. An empty page holds any receipt within the ceiling,
            // and the store refuses larger ones as rows to quarantine, so a
            // fresh page refusing one is the store contradicting itself.
            RecoveryStep::BudgetExhausted {
                declared_bytes,
                remaining,
            } if page.fresh() => {
                return Err(ProcessLossRecoveryError::ByteBudget {
                    declared: declared_bytes,
                    remaining,
                });
            }
            RecoveryStep::BudgetExhausted { .. } => page.next(report),
            RecoveryStep::Row {
                row,
                next_after,
                bytes_charged,
            } => {
                page.bytes = page.bytes.saturating_add(bytes_charged);
                report.bytes_read = report.bytes_read.saturating_add(bytes_charged);
                match reconcile_row(store, clock, ceiling, session, row, report) {
                    Err(error) if error.is_storage_failure() => return Err(error),
                    Err(error) => report.warn(Some(session), Some(&next_after), error.to_string()),
                    Ok(()) => {}
                }
                page.rows = page.rows.saturating_add(1);
                after_attempt = Some(next_after);
            }
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests;
