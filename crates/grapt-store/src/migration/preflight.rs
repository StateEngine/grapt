//! Read-only readiness classification before any write seam.
//!
//! Modified: separation of bounded SQLite validation
//! from selected-history loading. Copyright OpenAI, Apache-2.0.
//! See notices/10-adoptions/warm-store-readiness.md.

use crate::epoch::{validate_read_only, EpochError};
use crate::log::EventLog;
use crate::migrations::ProductionMigrations;
use crate::quarantine::{inspect, MigrationQuarantine, QuarantineError};
use rusqlite::ffi::ErrorCode;
use std::path::Path;

pub use super::preflight_types::{
    BackupCapacity, BackupDirectory, BackupPermissions, CreateBackupFile, FutureBackup,
    MigrationPlan, WriteBackupFile,
};

const QUICK_CHECK_OPERATION_BUDGET: i64 = 10_000_000;
const QUICK_CHECK_PROGRESS_INTERVAL: i32 = 1_000;
const CORRUPTION_DETAIL_LIMIT: usize = 256;

#[derive(Debug)]
pub enum StoreReadiness {
    Absent,
    Unreadable(Unreadable),
    MigrationNeeded(MigrationPlan),
    QuarantinedOrCorrupt(Obstruction),
    HotJournal,
    /// The metadata and compatibility checks passed, but the optional page walk
    /// did not finish inside its launch budget. The store is usable; the full
    /// walk belongs to the explicit check command.
    Slow,
    Ready,
}

#[derive(Debug)]
pub enum Unreadable {
    Metadata(std::io::Error),
    NotAFile,
    Replaced,
    Sqlite(rusqlite::Error),
}

#[derive(Debug)]
pub enum Obstruction {
    Epoch(EpochError),
    Integrity(String),
    Migration(super::MigrationError),
    Quarantine(MigrationQuarantine),
    QuarantineMarker(QuarantineError),
}

/// The failure form of the two low read sites: either a recoverable hot
/// journal, which the preflight boundary lifts to
/// `StoreReadiness::HotJournal` and never publishes, or an obstruction
/// that stands to be published. Private: no other module may mint a hot
/// journal through these seams.
#[derive(Debug)]
enum PreflightFailure {
    HotJournal,
    Obstruction(Obstruction),
}

/// Inspect metadata and compatibility for an ordinary ready-store open.
/// Historical pages are checked when read; this is not a whole-database audit.
/// Initialization, migration and explicit full inspection retain page validation.
pub fn preflight_metadata_read_only(path: &Path) -> StoreReadiness {
    super::authority::preflight_metadata_observed(path).readiness
}

pub fn preflight_read_only(path: &Path) -> StoreReadiness {
    super::authority::preflight_observed(path).readiness
}

/// The operations the launch classification may spend on its page walk.
///
/// Exposed so a witness can pin that the launch path stays bounded: an unbounded
/// launch walk would put a whole-database audit back on the launcher's readiness.
pub fn launch_walk_operations() -> Option<i64> {
    Validation::LAUNCH_WALK.launch_walk_operations()
}

/// A classification with an explicit page-walk bound, for witnesses that cannot
/// build a store large enough to exhaust the real launch budget.
pub fn preflight_with_walk_budget(path: &Path, operations: i64) -> StoreReadiness {
    if operations <= 0 {
        return StoreReadiness::Unreadable(Unreadable::Metadata(std::io::Error::other(
            "a page-walk budget must be positive",
        )));
    }
    super::preflight::preflight_unfenced(path, Validation::BoundedWalk { operations })
}

/// A hot rollback journal fails every read-only page read with SQLite's
/// `SQLITE_READONLY_ROLLBACK`: the pager demands the rollback of the
/// torn transaction, which a read-only connection cannot perform. That is
/// a recoverable state, not corruption — the code must survive
/// classification so the owned writable open can roll it back.
fn is_hot_journal_error(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.extended_code == rusqlite::ffi::SQLITE_READONLY_ROLLBACK
    )
}

/// How much page-walking a classification asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WalkBudget {
    Skip,
    Capped(i64),
    Uncapped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Validation {
    /// Metadata and compatibility only; historical pages are checked on read.
    Metadata,
    /// Walk the whole database, but stop after `operations` VM steps.
    BoundedWalk { operations: i64 },
    /// Walk the whole database to completion. Only for explicit, offline work.
    WholeDatabase,
}

impl Validation {
    /// The launch bound: enough for an ordinary store, short of a real audit.
    pub(crate) const LAUNCH_WALK: Self = Self::BoundedWalk {
        operations: QUICK_CHECK_OPERATION_BUDGET,
    };

    /// The same bound, narrowed for a witness that cannot build a store large
    /// enough to exhaust the real budget. Never used by production paths.
    #[doc(hidden)]
    pub fn launch_walk_within(operations: i64) -> Self {
        Self::BoundedWalk { operations }
    }

    /// The operations the launch walk may spend, when it is bounded at all.
    ///
    /// Exposed so a witness can pin that the launch path stays bounded: an
    /// unbounded launch walk would put a whole-database audit back on the
    /// launcher's readiness, which is the defect this change removes.
    #[doc(hidden)]
    pub fn launch_walk_operations(self) -> Option<i64> {
        match self {
            Self::BoundedWalk { operations } => Some(operations),
            _ => None,
        }
    }

    /// How far this validation walks. Only an explicit, offline caller may ask
    /// for the uncapped form.
    fn walk_budget(self) -> WalkBudget {
        match self {
            Self::Metadata => WalkBudget::Skip,
            Self::BoundedWalk { operations } => WalkBudget::Capped(operations),
            Self::WholeDatabase => WalkBudget::Uncapped,
        }
    }
}

pub(crate) fn preflight_unfenced(path: &Path, validation: Validation) -> StoreReadiness {
    let real = match super::authority::real_database_path(path) {
        Ok(real) => real,
        Err(error) => return StoreReadiness::Unreadable(Unreadable::Metadata(error)),
    };
    let path = real.as_path();
    let metadata = match path.metadata() {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return StoreReadiness::Unreadable(Unreadable::NotAFile),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return StoreReadiness::Absent;
        }
        Err(error) => return StoreReadiness::Unreadable(Unreadable::Metadata(error)),
    };
    let mut log = match EventLog::open_read_only(path) {
        Ok(log) => log,
        Err(crate::log::LogError::Sqlite(error)) if is_hot_journal_error(&error) => {
            return StoreReadiness::HotJournal;
        }
        Err(crate::log::LogError::Sqlite(error)) => {
            return StoreReadiness::Unreadable(Unreadable::Sqlite(error));
        }
        Err(crate::log::LogError::Path(error)) => {
            return StoreReadiness::Unreadable(Unreadable::Metadata(error));
        }
        Err(crate::log::LogError::Decode(_)) => {
            return StoreReadiness::QuarantinedOrCorrupt(Obstruction::Integrity(
                "event log rejected".into(),
            ));
        }
    };
    let connection = log.conn.get_mut();
    let mut slow = false;
    match walk_database(connection, validation.walk_budget()) {
        WalkOutcome::Skipped => {}
        WalkOutcome::Finished(DatabaseWalk::Clean) => {}
        // A spent budget says the check did not finish, never that the database
        // is damaged. The compatibility checks below still run: only a store
        // that passes them and owes no migration is a visible, slow readiness.
        WalkOutcome::Finished(DatabaseWalk::BudgetSpent) => slow = true,
        WalkOutcome::Failed(PreflightFailure::HotJournal) => return StoreReadiness::HotJournal,
        WalkOutcome::Failed(PreflightFailure::Obstruction(other)) => {
            return StoreReadiness::QuarantinedOrCorrupt(other)
        }
    }
    let event_table = match table_exists(connection, "event") {
        Ok(exists) => exists,
        Err(PreflightFailure::HotJournal) => return StoreReadiness::HotJournal,
        Err(PreflightFailure::Obstruction(obstruction)) => {
            return StoreReadiness::QuarantinedOrCorrupt(obstruction)
        }
    };
    if !event_table {
        return StoreReadiness::QuarantinedOrCorrupt(Obstruction::Integrity(
            "the durable event table is missing".into(),
        ));
    }
    match inspect(connection) {
        Ok(Some(quarantine)) => {
            return StoreReadiness::QuarantinedOrCorrupt(Obstruction::Quarantine(quarantine));
        }
        Ok(None) => {}
        Err(error) => {
            return StoreReadiness::QuarantinedOrCorrupt(Obstruction::QuarantineMarker(error));
        }
    }
    match validate_read_only(connection) {
        Ok(_) => {}
        Err(error) => return StoreReadiness::QuarantinedOrCorrupt(Obstruction::Epoch(error)),
    }
    let migrations = match ProductionMigrations::load() {
        Ok(migrations) => migrations,
        Err(error) => {
            return StoreReadiness::QuarantinedOrCorrupt(Obstruction::Migration(error));
        }
    };
    let pending = match super::plan_read_only(connection, migrations.as_slice().as_slice()) {
        Ok(pending) => pending,
        Err(error) => return StoreReadiness::QuarantinedOrCorrupt(Obstruction::Migration(error)),
    };
    if pending.is_empty() {
        return if slow {
            StoreReadiness::Slow
        } else {
            StoreReadiness::Ready
        };
    }
    StoreReadiness::MigrationNeeded(MigrationPlan {
        pending,
        backup: super::authority::future_backup(path, &metadata),
    })
}

fn table_exists(connection: &rusqlite::Connection, table: &str) -> Result<bool, PreflightFailure> {
    connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count != 0)
        .map_err(|error| {
            if is_hot_journal_error(&error) {
                PreflightFailure::HotJournal
            } else {
                PreflightFailure::Obstruction(Obstruction::Integrity(truncate_detail(
                    &error.to_string(),
                )))
            }
        })
}

/// What a page walk established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DatabaseWalk {
    Clean,
    /// The budget ran out before the walk finished. Not a verdict on the data.
    BudgetSpent,
}

/// What asking for a walk produced.
enum WalkOutcome {
    Skipped,
    Finished(DatabaseWalk),
    Failed(PreflightFailure),
}

fn walk_database(connection: &mut rusqlite::Connection, budget: WalkBudget) -> WalkOutcome {
    let operation_budget = match budget {
        WalkBudget::Skip => return WalkOutcome::Skipped,
        WalkBudget::Capped(operations) => Some(operations),
        WalkBudget::Uncapped => None,
    };
    if let Some(budget) = operation_budget {
        // SQLite calls the handler after each interval of VM steps. The interval
        // is never coarser than the budget, or a small budget would be rounded
        // away and a slow check would report a clean walk it never made.
        let interval = budget.clamp(1, i64::from(QUICK_CHECK_PROGRESS_INTERVAL));
        let mut operations_remaining = budget;
        let installed = connection.progress_handler(
            i32::try_from(interval).unwrap_or(QUICK_CHECK_PROGRESS_INTERVAL),
            Some(move || {
                operations_remaining = operations_remaining.saturating_sub(interval);
                operations_remaining <= 0
            }),
        );
        if installed.is_err() {
            return WalkOutcome::Failed(PreflightFailure::Obstruction(Obstruction::Integrity(
                "quick check could not install its operation budget".into(),
            )));
        }
    }
    let check = (|| {
        let mut statement = connection.prepare("PRAGMA quick_check(1)")?;
        let mut rows = statement.query([])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let result: String = row.get(0)?;
        Ok(Some(result))
    })();
    if operation_budget.is_some()
        && connection
            .progress_handler(0, None::<fn() -> bool>)
            .is_err()
    {
        return WalkOutcome::Failed(PreflightFailure::Obstruction(Obstruction::Integrity(
            "quick check could not remove its operation budget".into(),
        )));
    }
    match check {
        Ok(Some(result)) if result == "ok" => WalkOutcome::Finished(DatabaseWalk::Clean),
        Ok(Some(result)) => WalkOutcome::Failed(PreflightFailure::Obstruction(
            Obstruction::Integrity(truncate_detail(&result)),
        )),
        Ok(None) => WalkOutcome::Failed(PreflightFailure::Obstruction(Obstruction::Integrity(
            "quick check returned no verdict".into(),
        ))),
        // The bound stopped the walk, so it has no verdict to report. This is
        // the whole point of the split: a slow check is not a corrupt store.
        Err(rusqlite::Error::SqliteFailure(error, _))
            if error.code == ErrorCode::OperationInterrupted && operation_budget.is_some() =>
        {
            WalkOutcome::Finished(DatabaseWalk::BudgetSpent)
        }
        Err(error) if is_hot_journal_error(&error) => {
            WalkOutcome::Failed(PreflightFailure::HotJournal)
        }
        Err(error) => WalkOutcome::Failed(PreflightFailure::Obstruction(Obstruction::Integrity(
            truncate_detail(&error.to_string()),
        ))),
    }
}

fn truncate_detail(detail: &str) -> String {
    if detail.len() <= CORRUPTION_DETAIL_LIMIT {
        return detail.to_owned();
    }
    let mut bounded = detail
        .chars()
        .take(CORRUPTION_DETAIL_LIMIT)
        .collect::<String>();
    bounded.push('~');
    bounded
}
