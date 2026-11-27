//! Atomic current schema, only for the retained exclusive create-new handle.
use super::*;
use crate::migration::authority::DatabaseFileHandle;
use crate::{MigrationError, ProductionMigrations};
use grapt_types::ClockAuthority;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FreshBoundary {
    SqliteOpened,
    MigrationRecorded(usize),
    BeforeCommit,
    AfterCommit,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum FreshOpenError {
    #[error(transparent)]
    Existing(#[from] ExistingOpenError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Open(#[from] WriteError),
    #[error(transparent)]
    Migration(#[from] MigrationError),
    #[error("exclusive fresh database no longer has an empty schema")]
    NotEmpty,
    #[error("fresh schema violates a foreign key")]
    ForeignKey,
}

impl EventStore {
    /// Crate-private: only the owned initializer passes a retained create_new
    /// handle. Existing files never enter this no-backup transaction.
    pub(crate) fn open_created_with_authorities(
        path: &Path,
        handle: &DatabaseFileHandle,
        engine_version: &str,
        clock: &dyn ClockAuthority,
        ids: &dyn IdAuthority,
        mut hook: impl FnMut(FreshBoundary) -> Result<(), MigrationError>,
    ) -> Result<Self, FreshOpenError> {
        handle.revalidate().map_err(ExistingOpenError::Identity)?;
        Self::require_file_identity(path, handle.identity())?;
        let mut conn = handle.open_sqlite(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        hook(FreshBoundary::SqliteOpened)?;
        // Path-based VFS opens (macOS/Windows) must still name the retained
        // identity before even starting writable schema preparation.
        handle.revalidate().map_err(ExistingOpenError::Identity)?;
        Self::require_file_identity(path, handle.identity())?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        Self::enforce_foreign_keys(&conn)?;
        let migrations = ProductionMigrations::load()?;
        let mut budget = crate::session_list::install_vm_budget(
            &mut conn,
            crate::session_list::SqliteVmBudget::open_verification(),
        )?;
        let deferred = budget.suspend();
        let tx = budget.begin_immediate()?;
        // Recheck inside the write lock. Zero event rows or a ready-looking
        // existing file are never evidence of exclusive creation.
        let objects: i64 =
            tx.query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| row.get(0))?;
        let epoch: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if objects != 0 || epoch != 0 {
            return Err(FreshOpenError::NotEmpty);
        }
        crate::epoch::stamp_within(&tx, engine_version)?;
        // Keep the existing epoch/schema owner; injected time replaces only
        // this newly created row's time inside the same uncommitted transaction.
        tx.execute(
            "UPDATE engine_epoch SET time = ?1",
            [clock.now().as_millis()],
        )?;
        Self::ensure_schema(&tx, &deferred)?;
        // Journal shape is the existing migration owner's exact declaration.
        tx.execute(
            "CREATE TABLE IF NOT EXISTS rust_migration (
                    id TEXT PRIMARY KEY,
                    time_completed INTEGER NOT NULL
                )",
            [],
        )?;
        crate::quarantine::ensure_table(&tx)?;
        for (position, migration) in migrations.as_slice().into_iter().enumerate() {
            migration
                .validate(&tx)
                .and_then(|()| migration.up(&tx))
                .map_err(|message| MigrationError::Failed {
                    id: migration.id(),
                    message,
                })?;
            tx.execute(
                "INSERT INTO rust_migration (id, time_completed) VALUES (?1, ?2)",
                rusqlite::params![migration.id().as_str(), clock.now().as_millis()],
            )?;
            hook(FreshBoundary::MigrationRecorded(position))?;
        }
        // Event-table migrations rebuild their indexes. Finish auxiliary
        // preparation before publishing the fresh store's atomic commit.
        Self::ensure_probe_namespace_indexes(&tx)?;
        Self::verify_index_shapes(&tx)?;
        let invalid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)",
            [],
            |row| row.get(0),
        )?;
        if invalid {
            return Err(FreshOpenError::ForeignKey);
        }
        hook(FreshBoundary::BeforeCommit)?;
        handle.revalidate().map_err(ExistingOpenError::Identity)?;
        Self::require_file_identity(path, handle.identity())?;
        tx.commit()?;
        budget.finish()?;
        hook(FreshBoundary::AfterCommit)?;
        handle.revalidate().map_err(ExistingOpenError::Identity)?;
        Self::require_file_identity(path, handle.identity())?;
        // Fresh schema creation retains its atomic rollback transaction. Only
        // the admitted, committed store switches to the live WAL policy.
        journal::configure(&conn)?;
        Ok(Self::guarded(conn, ids)?)
    }
}
