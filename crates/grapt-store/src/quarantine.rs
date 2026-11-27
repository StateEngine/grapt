//! The store-owned between-migrations quarantine marker.
//!
//! # The state this exists to represent
//!
//! Crash-resumability is already proven PER MIGRATION: `migration::apply`
//! applies and records each migration in one transaction, so a crash leaves
//! either both or neither. What had no representation is the state BETWEEN
//! migrations. A database with migrations 1-2 recorded and 3 pending is
//! half-migrated, and nothing marked it as not-current-truth: `EventStore::open`
//! would either succeed — because the shape it happened to reach satisfies
//! `verify_columns` and the target-shape probe — or fail with a schema refusal
//! naming a COLUMN, which reads as an alien database rather than as an
//! interrupted migration pointing at its own backup.
//!
//! **A half-migrated database that opens successfully is the defect this marker
//! exists to prevent.**
//!
//! # Why a store-owned type
//!
//! `grapt_types::terminality::DeliveryTerminal` already has a `Quarantined`
//! variant. It is an OPERATION DELIVERY concept — a notification debt that
//! could not be settled — and it has nothing to do with a database whose schema
//! conversion was interrupted. Reusing it would create a fourth cross-domain
//! namespace collision of exactly the kind this project has already recorded
//! three times (`security` / `types::service` / `location` capabilities). The
//! marker is therefore store-owned and `grapt-types` is untouched.
//!
//! # What quarantine authorizes
//!
//! Detection, refusal and RESUME. Nothing else. It never re-drives a migration
//! automatically, never adopts an ambiguous partial effect as complete, and
//! never repairs the database. Re-running the production migration set on a
//! quarantined database completes it, because the journal already records what
//! landed and `validate_and_plan` computes the remainder — the marker adds the
//! refusal, not the recovery.

use grapt_types::{BackupId, MigrationId};
use rusqlite::Connection;

/// A migration run that started and did not finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationQuarantine {
    pending: Vec<MigrationId>,
    backup: BackupId,
}

impl MigrationQuarantine {
    /// The migrations this run still owed when it was interrupted.
    pub fn pending(&self) -> &[MigrationId] {
        &self.pending
    }

    /// The verified rollback point taken before the run mutated anything.
    pub fn backup(&self) -> &BackupId {
        &self.backup
    }

    /// The refusal text an open reports, naming the debt and the rollback point.
    pub(crate) fn describe(&self) -> String {
        let pending = self
            .pending
            .iter()
            .map(MigrationId::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "migration run did not complete: {} still pending [{pending}]; \
             the verified rollback point is {}",
            self.pending.len(),
            self.backup.as_str()
        )
    }
}

/// A marker row that cannot be decoded is corruption, not an absent marker.
///
/// Fail CLOSED: an unreadable marker refuses the open. Treating it as absent
/// would let a damaged row silently re-admit a half-migrated database, which is
/// the one thing this module must never do.
#[derive(Debug, thiserror::Error)]
pub enum QuarantineError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("migration quarantine marker is unreadable: {reason}; refusing to open")]
    Corrupt { reason: String },
}

const TABLE: &str = "rust_migration_run";

/// Creates the marker table. Called from the SAME first transaction that
/// creates the journal, so a database can never carry a journal without the
/// place its interruption would be recorded.
pub(crate) fn ensure_table(tx: &rusqlite::Transaction<'_>) -> Result<(), rusqlite::Error> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS rust_migration_run (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            started_ms INTEGER NOT NULL,
            pending TEXT NOT NULL,
            backup TEXT NOT NULL
        );",
    )
}

/// Enters quarantine, in the first migration's own transaction.
///
/// `INSERT OR REPLACE` because a RESUME legitimately finds the crashed run's
/// marker already there and owes a smaller remainder than it did.
pub(crate) fn enter(
    tx: &rusqlite::Transaction<'_>,
    pending: &[MigrationId],
    backup: &BackupId,
    started_ms: i64,
) -> Result<(), rusqlite::Error> {
    let joined = pending
        .iter()
        .map(MigrationId::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    tx.execute(
        "INSERT OR REPLACE INTO rust_migration_run (id, started_ms, pending, backup)
         VALUES (1, ?1, ?2, ?3)",
        rusqlite::params![started_ms, joined, backup.as_str()],
    )?;
    Ok(())
}

/// Leaves quarantine, in the LAST migration's own transaction.
///
/// Same transaction as the final `apply + record`, so the marker and the work
/// it covers settle together: there is no instant in which every migration is
/// recorded but the database still reads as quarantined, and none in which the
/// marker is gone while work remains.
pub(crate) fn leave(tx: &rusqlite::Transaction<'_>) -> Result<(), rusqlite::Error> {
    tx.execute("DELETE FROM rust_migration_run", [])?;
    Ok(())
}

/// Whether this database is mid-migration.
///
/// Read-only on every path. An absent TABLE is an absent marker — a database
/// that never met the Rust migration path is not quarantined.
pub fn inspect(conn: &Connection) -> Result<Option<MigrationQuarantine>, QuarantineError> {
    let table: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [TABLE],
        |r| r.get(0),
    )?;
    if table == 0 {
        return Ok(None);
    }
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT pending, backup FROM rust_migration_run WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map(Some)
        .or_else(|e| {
            if e == rusqlite::Error::QueryReturnedNoRows {
                Ok(None)
            } else {
                Err(e)
            }
        })?;
    let Some((pending, backup)) = row else {
        return Ok(None);
    };
    let backup = BackupId::new(backup).ok_or_else(|| QuarantineError::Corrupt {
        reason: "the marker names an empty backup identity".to_owned(),
    })?;
    let mut ids = Vec::new();
    for raw in pending.split('\n') {
        ids.push(
            MigrationId::new(raw).ok_or_else(|| QuarantineError::Corrupt {
                reason: "the marker's pending list contains an empty identity".to_owned(),
            })?,
        );
    }
    Ok(Some(MigrationQuarantine {
        pending: ids,
        backup,
    }))
}
