//! Migration scaffolding — typed, backup-first, crash-safe, idempotent.
//!
//! Sequence law (mutation-free until the backup exists):
//! 1. READ-ONLY validation — epoch (foreign/corrupt refuses), duplicate
//!    supplied ids refuse, recorded ids unknown to this build refuse
//!    (a newer engine touched this database), pending list computed.
//! 2. Nothing pending → `AlreadyCurrent`, no backup churn.
//! 3. Consistent snapshot via the SQLite backup API (page-consistent under
//!    the source lock — never an unlocked raw file copy) to a CREATE-NEW
//!    unique destination; an existing rollback point is never overwritten.
//!    The backup is integrity-checked and fingerprinted.
//! 4. The FIRST transaction atomically: stamps the engine epoch (if oracle-era),
//!    creates the journal, applies migration #1, records it. Failure leaves
//!    epoch, schema, and journal untouched. Later migrations commit one
//!    transaction each (apply + record together): crash-resumable, never
//!    half-recorded.
//!
//! Rollback honesty (§G.4): the receipt is the artifact the R6 rehearsal
//! starts from — restore-from-backup (losing Rust-era data, recorded and
//! signed off) or forward re-export. Nothing here pretends a lossless
//! in-place downgrade exists.

use grapt_types::{BackupId, ClockAuthority, DatabaseFingerprint, MigrationId};
use rusqlite::{Connection, TransactionBehavior};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

pub(crate) mod authority;
mod preflight_types;

pub use authority::{BoundDatabaseError, BoundDatabaseFile};

#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Epoch(#[from] crate::epoch::EpochError),
    #[error("backup io: {0}")]
    Backup(#[from] std::io::Error),
    #[error("backup destination already exists: {0}")]
    BackupDestinationExists(PathBuf),
    #[error("backup failed integrity check: {0}")]
    BackupIntegrity(String),
    #[error("duplicate migration id supplied: {0}")]
    DuplicateSupplied(MigrationId),
    #[error("database records migration {0} unknown to this build; a newer engine owns it")]
    UnknownRecorded(MigrationId),
    #[error("migration {id} failed: {message}")]
    Failed { id: MigrationId, message: String },
    #[error("snapshot became stale: another connection committed between validation/backup and migration; refusing without mutation")]
    SnapshotStale,
    #[error("the authorized migration plan no longer matches the database")]
    AuthorizedPlanStale,
    #[error("the observed backup location is not authorized for this migration")]
    BackupAuthorityRefused,
    #[error("the database path no longer names the classified file")]
    PathIdentityChanged,
    #[error("database file identity could not be read: {0}")]
    FileIdentity(std::io::Error),
    #[error(transparent)]
    Quarantine(#[from] crate::quarantine::QuarantineError),
    /// A production migration's identity literal did not satisfy
    /// [`MigrationId`]'s non-empty rule.
    ///
    /// Structurally unreachable — the literals in [`crate::migrations`] are
    /// non-empty constants — but REPRESENTED rather than resolved by an
    /// assertion, because this crate denies `unwrap`/`expect` precisely so an
    /// impossible branch refuses instead of aborting.
    #[error("migration identity literal {literal:?} is not a valid MigrationId")]
    InvalidMigrationIdentity { literal: &'static str },
    /// Produced only by the `cfg(test)`-sealed crash-cut seam.
    #[cfg(test)]
    #[error("migration run interrupted at the {0} cut point")]
    Interrupted(&'static str),
}

/// Typed proof that a verified, consistent rollback point exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupReceipt {
    pub id: BackupId,
    pub source: PathBuf,
    pub backup: PathBuf,
    pub bytes: u64,
    pub fingerprint: DatabaseFingerprint,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MigrationOutcome {
    /// Every supplied migration is already recorded; nothing ran, no backup.
    AlreadyCurrent,
    /// `applied` migrations ran, in order, behind a verified backup.
    Applied {
        applied: Vec<MigrationId>,
        backup: BackupReceipt,
    },
}

/// One schema migration. `up` runs inside the transaction that also records
/// the id — the two commit or roll back together.
pub trait Migration {
    fn id(&self) -> MigrationId;
    /// Read-only preflight executed before a rollback backup is created.
    /// A migration with bounded source requirements must refuse here, not
    /// after backup creation or after its transaction has begun.
    fn validate(&self, _conn: &Connection) -> Result<(), String> {
        Ok(())
    }
    fn up(&self, tx: &rusqlite::Transaction<'_>) -> Result<(), String>;
}

fn fingerprint_file(path: &Path) -> Result<(u64, DatabaseFingerprint), MigrationError> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 1 << 16];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    let hex = format!("{:x}", hasher.finalize());
    let fingerprint = DatabaseFingerprint::new(hex)
        .ok_or_else(|| MigrationError::BackupIntegrity("empty fingerprint".into()))?;
    Ok((total, fingerprint))
}

/// Read-only validation: refuses before any mutation, computes the pending
/// list. The journal may legitimately be absent on an oracle-era database.
fn validate_and_plan(
    conn: &Connection,
    migrations: &[&dyn Migration],
) -> Result<Vec<usize>, MigrationError> {
    crate::epoch::validate_read_only(conn)?;

    let mut supplied = HashSet::new();
    for m in migrations {
        if !supplied.insert(m.id()) {
            return Err(MigrationError::DuplicateSupplied(m.id()));
        }
    }

    let journal_exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'rust_migration'",
        [],
        |r| r.get(0),
    )?;
    let mut recorded = HashSet::new();
    if journal_exists != 0 {
        let mut stmt = conn.prepare("SELECT id FROM rust_migration")?;
        let ids = stmt.query_map([], |r| r.get::<_, String>(0))?;
        for id in ids {
            let id = id?;
            let id = MigrationId::new(id.clone()).ok_or_else(|| {
                MigrationError::BackupIntegrity(format!("empty journal id {id:?}"))
            })?;
            if !supplied.contains(&id) {
                return Err(MigrationError::UnknownRecorded(id));
            }
            recorded.insert(id);
        }
    }
    let pending = migrations
        .iter()
        .enumerate()
        .filter(|(_, m)| !recorded.contains(&m.id()))
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    for index in &pending {
        let migration = migrations[*index];
        migration
            .validate(conn)
            .map_err(|message| MigrationError::Failed {
                id: migration.id(),
                message,
            })?;
    }
    Ok(pending)
}

/// Read-only migration planning for readiness surfaces: the same validation
/// and pending computation as the writer, with identities instead of indexes.
pub(crate) fn plan_read_only(
    conn: &Connection,
    migrations: &[&dyn Migration],
) -> Result<Vec<MigrationId>, MigrationError> {
    validate_and_plan(conn, migrations).map(|pending| {
        pending
            .into_iter()
            .map(|index| migrations[index].id())
            .collect()
    })
}

fn verify_backup(path: &Path) -> Result<(u64, DatabaseFingerprint), MigrationError> {
    let backup = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let ok: String = backup.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if ok != "ok" {
        return Err(MigrationError::BackupIntegrity(ok));
    }
    drop(backup);
    fingerprint_file(path)
}

/// Consistent snapshot via the SQLite backup API to a create-new destination.
fn take_backup(
    src: &Connection,
    db_path: &Path,
    backup_dir: &Path,
    clock: &dyn ClockAuthority,
) -> Result<BackupReceipt, MigrationError> {
    std::fs::create_dir_all(backup_dir)?;
    let file_name = db_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let base = format!(
        "{file_name}.pre-rust-migration.{}.{}",
        clock.now().as_millis(),
        std::process::id(),
    );
    let mut claimed = None;
    for suffix in 0_u32..=1024 {
        let candidate = if suffix == 0 {
            format!("{base}.bak")
        } else {
            format!("{base}-{suffix}.bak")
        };
        let candidate_path = backup_dir.join(&candidate);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate_path)
        {
            Ok(_file) => {
                claimed = Some((candidate, candidate_path));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    let (unique, backup_path) = match claimed {
        Some(pair) => pair,
        None => {
            return Err(MigrationError::BackupDestinationExists(
                backup_dir.join(format!("{base}.bak")),
            ));
        }
    };
    let copied = (|| -> Result<(), MigrationError> {
        let mut dst = Connection::open(&backup_path)?;
        let backup = rusqlite::backup::Backup::new(src, &mut dst)?;
        backup.run_to_completion(64, std::time::Duration::from_millis(5), None)?;
        Ok(())
    })();
    if let Err(error) = copied {
        let _ = std::fs::remove_file(&backup_path);
        return Err(error);
    }
    let (bytes, fingerprint) = verify_backup(&backup_path)?;
    let id = BackupId::new(unique)
        .ok_or_else(|| MigrationError::BackupIntegrity("empty backup id".into()))?;
    Ok(BackupReceipt {
        id,
        source: db_path.to_owned(),
        backup: backup_path,
        bytes,
        fingerprint,
    })
}

/// Apply pending migrations to the database at `db_path`.
///
/// The journal timestamp comes from the named production clock adapter. Use
/// [`apply_with_clock`] to inject a deterministic one.
pub fn apply(
    db_path: &Path,
    backup_dir: &Path,
    engine_version: &str,
    migrations: &[&dyn Migration],
) -> Result<MigrationOutcome, MigrationError> {
    apply_with_clock(
        db_path,
        backup_dir,
        engine_version,
        migrations,
        &crate::clock::SystemClock,
    )
}

/// [`apply`] under an injected clock authority.
///
/// `rust_migration.time_completed` is a DURABLE row. It used to be written by
/// `strftime('%s','now') * 1000` inside the INSERT — an ambient clock read that
/// `clippy::disallowed_methods` cannot see, because that gate matches Rust call
/// paths and this one lived in SQL text. Row-11 law does not stop at the
/// language boundary, so both the journal timestamp and backup filename now
/// come from the injected [`grapt_types::ClockAuthority`].
pub fn apply_with_clock(
    db_path: &Path,
    backup_dir: &Path,
    engine_version: &str,
    migrations: &[&dyn Migration],
    clock: &dyn ClockAuthority,
) -> Result<MigrationOutcome, MigrationError> {
    apply_inner(
        db_path,
        backup_dir,
        engine_version,
        migrations,
        clock,
        Interrupt::None,
    )
}

/// cfg(test)-sealed crash-cut seam.
///
/// Every variant that injects behaviour is unnameable in production builds, so
/// no production caller can interrupt a migration run or inject a concurrent
/// mutator between backup and migration.
enum Interrupt {
    None,
    /// Runs a hook between the backup and the first migration transaction.
    #[cfg(test)]
    AfterBackup(Box<dyn FnOnce()>),
    /// Stops before the first migration transaction opens — the cut point
    /// AFTER the backup exists and BEFORE any marker.
    #[cfg(test)]
    BeforeFirstTransaction,
    /// Stops after `up` returned but before the journal row and marker settle,
    /// so the transaction rolls back.
    #[cfg(test)]
    BeforeCommitOf(usize),
    /// Stops after the given migration's transaction commits — the true
    /// between-migrations state.
    #[cfg(test)]
    AfterCommitOf(usize),
}

impl Interrupt {
    fn after_backup(self) -> Self {
        match self {
            #[cfg(test)]
            Interrupt::AfterBackup(hook) => {
                hook();
                Interrupt::None
            }
            other => other,
        }
    }

    #[cfg(test)]
    fn stops_before_commit_of(&self, position: usize) -> bool {
        matches!(self, Interrupt::BeforeCommitOf(cut) if *cut == position)
    }

    #[cfg(test)]
    fn stops_after_commit_of(&self, position: usize) -> bool {
        matches!(self, Interrupt::AfterCommitOf(cut) if *cut == position)
    }

    #[cfg(test)]
    fn stops_before_first_transaction(&self) -> bool {
        matches!(self, Interrupt::BeforeFirstTransaction)
    }
}

/// A stale snapshot's backup file must never survive as an apparent rollback
/// point: remove it
/// best-effort, then return ONLY the typed refusal.
fn refuse_stale(backup: &BackupReceipt) -> MigrationError {
    let _ = std::fs::remove_file(&backup.backup);
    MigrationError::SnapshotStale
}

fn data_version(conn: &Connection) -> Result<i64, MigrationError> {
    Ok(conn.query_row("PRAGMA data_version", [], |r| r.get(0))?)
}

/// `after_backup` is a test seam: it runs between the backup and the first
/// migration transaction, where a concurrent committer can invalidate the
/// snapshot (review round 2, item F). Production passes a no-op.
fn apply_inner(
    db_path: &Path,
    backup_dir: &Path,
    engine_version: &str,
    migrations: &[&dyn Migration],
    clock: &dyn ClockAuthority,
    interrupt: Interrupt,
) -> Result<MigrationOutcome, MigrationError> {
    apply_inner_with_authorization(
        db_path,
        backup_dir,
        engine_version,
        migrations,
        clock,
        authority::WritableTarget::Path,
        interrupt,
    )
}

pub(crate) fn apply_bound_with_clock(
    db_path: &Path,
    backup_dir: &Path,
    engine_version: &str,
    migrations: &[&dyn Migration],
    clock: &dyn ClockAuthority,
    handle: &authority::DatabaseFileHandle,
) -> Result<MigrationOutcome, MigrationError> {
    apply_inner_with_authorization(
        db_path,
        backup_dir,
        engine_version,
        migrations,
        clock,
        authority::WritableTarget::Bound(handle),
        Interrupt::None,
    )
}

fn apply_inner_with_authorization(
    db_path: &Path,
    backup_dir: &Path,
    engine_version: &str,
    migrations: &[&dyn Migration],
    clock: &dyn ClockAuthority,
    target: authority::WritableTarget<'_>,
    interrupt: Interrupt,
) -> Result<MigrationOutcome, MigrationError> {
    let authorized = target.authorization();
    let bound = target.handle();
    if let Some(authorized) = authorized {
        authority::validate_before_sqlite_open(db_path, authorized)?;
    }
    if let Some(handle) = bound {
        authority::validate_bound_path(db_path, handle)?;
    }
    let mut conn = match bound {
        Some(handle) => handle.open_sqlite(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?,
        None => Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?,
    };
    if let Some(handle) = bound {
        authority::validate_bound_path(db_path, handle)?;
    }

    // Phase 1: read-only. Every refusal path leaves the database untouched.
    // PRAGMA data_version is the staleness fence: it changes when ANY OTHER
    // connection commits, so the plan/backup/migrate sequence can prove no
    // foreign commit slipped between its phases.
    let fence = data_version(&conn)?;
    let pending = validate_and_plan(&conn, migrations)?;
    if let Some(authorized) = authorized {
        authority::validate_authorization(db_path, migrations, &pending, authorized)?;
    }
    if pending.is_empty() {
        return Ok(MigrationOutcome::AlreadyCurrent);
    }

    // Phase 2: verified consistent rollback point, before ANY mutation.
    let backup = take_backup(&conn, db_path, backup_dir, clock)?;
    if data_version(&conn)? != fence {
        return Err(refuse_stale(&backup));
    }
    #[cfg_attr(not(test), allow(unused_variables))]
    let interrupt = interrupt.after_backup();
    // CUT POINT: after a verified rollback point exists, before any marker.
    // The database is still byte-for-byte pre-migration here.
    #[cfg(test)]
    if interrupt.stops_before_first_transaction() {
        return Err(MigrationError::Interrupted("before-first-transaction"));
    }

    // Phase 3: first transaction atomically stamps (if oracle-era) + creates the
    // journal and the quarantine marker + applies/records migration #1.
    let mut applied = Vec::new();
    let mut first = true;
    for (position, &index) in pending.iter().enumerate() {
        let migration = migrations[index];
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if first {
            // Re-fence and re-plan INSIDE the first Immediate transaction:
            // the backup must correspond to exactly the state being migrated.
            if data_version(&tx)? != fence {
                return Err(refuse_stale(&backup));
            }
            if validate_and_plan(&tx, migrations)? != pending {
                return Err(refuse_stale(&backup));
            }
            if let Some(authorized) = authorized {
                if let Err(error) =
                    authority::validate_authorization(db_path, migrations, &pending, authorized)
                {
                    let _ = std::fs::remove_file(&backup.backup);
                    return Err(error);
                }
            }
            let ts_era = crate::epoch::validate_read_only(&tx)?.is_none();
            if ts_era {
                crate::epoch::stamp_within(&tx, engine_version)?;
            }
            tx.execute(
                "CREATE TABLE IF NOT EXISTS rust_migration (
                    id TEXT PRIMARY KEY,
                    time_completed INTEGER NOT NULL
                )",
                [],
            )?;
            // The marker table is created in the SAME transaction as the
            // journal, so a database can never carry a journal without the
            // place its interruption would be recorded.
            crate::quarantine::ensure_table(&tx)?;
            first = false;
        }
        migration
            .up(&tx)
            .map_err(|message| MigrationError::Failed {
                id: migration.id(),
                message,
            })?;

        // CUT POINT: `up` succeeded but nothing has settled. Dropping the
        // transaction rolls back the schema change, the journal row and the
        // marker together — the same durable outcome as an interruption
        // between the rebuild's own steps, which is why those two cut points
        // share one witness rather than claiming separate attribution.
        #[cfg(test)]
        if interrupt.stops_before_commit_of(position) {
            drop(tx);
            return Err(MigrationError::Interrupted("before-commit"));
        }

        tx.execute(
            "INSERT INTO rust_migration (id, time_completed) VALUES (?1, ?2)",
            rusqlite::params![migration.id().as_str(), clock.now().as_millis()],
        )?;
        // SETTLE THE RUN'S OUTSTANDING DEBT, in the SAME transaction as the
        // work that just landed. The marker records the remainder AFTER this
        // migration, so it always names what is genuinely still owed rather
        // than what the run set out to do — which is what a resumed run and a
        // refused open both need to be truthful about.
        //
        // Because the marker settles with the work, there is no instant in
        // which a schema change is durable while the database reads as clean,
        // and none in which the marker survives a rollback that landed nothing.
        // The LAST migration's remainder is empty, and that is the only thing
        // that clears it.
        let remaining: Vec<MigrationId> = pending[position + 1..]
            .iter()
            .map(|&i| migrations[i].id())
            .collect();
        if remaining.is_empty() {
            crate::quarantine::leave(&tx)?;
        } else {
            crate::quarantine::enter(&tx, &remaining, &backup.id, clock.now().as_millis())?;
        }
        tx.commit()?;
        applied.push(migration.id());

        // CUT POINT: this migration is durable and the next is not. The marker
        // survives, so the next open refuses and names the remainder.
        #[cfg(test)]
        if interrupt.stops_after_commit_of(position) {
            return Err(MigrationError::Interrupted("after-commit"));
        }
    }
    Ok(MigrationOutcome::Applied { applied, backup })
}

pub mod preflight;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod readiness_tests {
    use super::preflight::{
        preflight_read_only, BackupPermissions, CreateBackupFile, MigrationPlan, Obstruction,
        StoreReadiness, Unreadable, WriteBackupFile,
    };
    use crate::{apply_migrations, EventLog, ProductionMigrations};
    use rusqlite::Connection;
    use std::path::{Path, PathBuf};

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    const TYPESCRIPT_ERA_SCHEMA: &str = "
    CREATE TABLE event_sequence (
        aggregate_id text PRIMARY KEY,
        seq integer NOT NULL,
        owner_id text
    );
    CREATE TABLE event (
        id text PRIMARY KEY,
        aggregate_id text NOT NULL,
        seq integer NOT NULL,
        created integer NOT NULL,
        type text NOT NULL,
        data text NOT NULL
    );
    CREATE TABLE session (
        id text PRIMARY KEY,
        project_id text NOT NULL,
        workspace_id text,
        parent_id text,
        directory text NOT NULL,
        title text NOT NULL,
        time_updated integer NOT NULL
    );
    ";

    fn fresh_root(label: &str) -> TestResult<PathBuf> {
        let root = std::env::temp_dir().join(label);
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        std::fs::create_dir_all(&root)?;
        Ok(root)
    }

    fn legacy_database(root: &Path, label: &str) -> TestResult<(PathBuf, Vec<u8>)> {
        let path = root.join(format!("{label}.db"));
        let connection = Connection::open(&path)?;
        connection.execute_batch(TYPESCRIPT_ERA_SCHEMA)?;
        connection.execute_batch(
            "
            INSERT INTO event_sequence VALUES ('ses_alpha', 0, NULL);
            INSERT INTO event VALUES
                ('evt_alpha', 'ses_alpha', 0, 1, 'session.created.1', '{}');
            INSERT INTO session VALUES
                ('ses_alpha', 'project', NULL, NULL, 'E:/w', 'Session', 1);
            ",
        )?;
        drop(connection);
        let bytes = std::fs::read(&path)?;
        Ok((path, bytes))
    }

    fn assert_migration_needed(readiness: StoreReadiness) -> MigrationPlan {
        match readiness {
            StoreReadiness::MigrationNeeded(plan) => plan,
            other => panic!("expected migration-needed readiness, found {other:?}"),
        }
    }

    fn assert_corrupt(readiness: StoreReadiness) {
        assert!(
            matches!(
                readiness,
                StoreReadiness::QuarantinedOrCorrupt(Obstruction::Integrity(_))
            ),
            "expected corrupt readiness, found {readiness:?}"
        );
    }

    fn backup_count(root: &Path) -> TestResult<usize> {
        let mut count = 0;
        for entry in std::fs::read_dir(root)? {
            if entry?.file_name().to_string_lossy().ends_with(".bak") {
                count += 1;
            }
        }
        Ok(count)
    }

    #[test]
    fn migration_needed_names_the_plan_and_does_not_write() -> TestResult {
        let root = fresh_root("ia1-b-migration-needed")?;
        let (database, before) = legacy_database(&root, "migration")?;
        let plan = assert_migration_needed(preflight_read_only(&database));
        assert_eq!(
            plan.pending.len(),
            ProductionMigrations::load()?.as_slice().len()
        );
        assert_eq!(
            plan.backup.location,
            database.parent().unwrap_or(Path::new(""))
        );
        assert_eq!(plan.backup.minimum_free_bytes, u64::try_from(before.len())?);
        assert_eq!(
            plan.backup.permissions,
            BackupPermissions {
                create_file: CreateBackupFile::Authorized,
                write_file: WriteBackupFile::Authorized,
            }
        );
        assert_eq!(backup_count(&root)?, 0);
        assert_eq!(std::fs::read(&database)?, before);
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn a_migrated_store_is_ready_without_further_writes() -> TestResult {
        let root = fresh_root("ia1-b-ready")?;
        let (database, _) = legacy_database(&root, "ready")?;
        assert_migration_needed(preflight_read_only(&database));
        let migrations = ProductionMigrations::load()?;
        let backup_dir = root.join("backups");
        apply_migrations(
            &database,
            &backup_dir,
            "ia1-b-test",
            migrations.as_slice().as_slice(),
        )?;
        let before = std::fs::read(&database)?;
        assert!(matches!(
            preflight_read_only(&database),
            StoreReadiness::Ready
        ));
        assert_eq!(std::fs::read(&database)?, before);
        let connection =
            Connection::open_with_flags(&database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        assert_eq!(version, 2);
        drop(connection);
        assert_eq!(backup_count(&backup_dir)?, 1);
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn absent_and_unreadable_paths_never_create_or_repair() -> TestResult {
        let root = fresh_root("ia1-b-refusals")?;
        let absent = root.join("absent.db");
        assert!(matches!(
            preflight_read_only(&absent),
            StoreReadiness::Absent
        ));
        assert!(!absent.exists());
        let directory = root.join("directory.db");
        std::fs::create_dir(&directory)?;
        assert!(matches!(
            preflight_read_only(&directory),
            StoreReadiness::Unreadable(Unreadable::NotAFile)
        ));
        let direct_absent = root.join("direct-absent.db");
        assert!(EventLog::open_read_only(&direct_absent).is_err());
        assert!(!direct_absent.exists());
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn corrupt_bytes_and_foreign_epochs_are_typed_without_writes() -> TestResult {
        let root = fresh_root("ia1-b-corrupt")?;
        let text = root.join("text.db");
        std::fs::write(&text, b"definitely not SQLite")?;
        let before = std::fs::read(&text)?;
        assert_corrupt(preflight_read_only(&text));
        assert_eq!(std::fs::read(&text)?, before);
        let (foreign, _) = legacy_database(&root, "foreign")?;
        Connection::open(&foreign)?.execute_batch("PRAGMA user_version = 99")?;
        let before = std::fs::read(&foreign)?;
        match preflight_read_only(&foreign) {
            StoreReadiness::QuarantinedOrCorrupt(Obstruction::Epoch(_)) => {}
            other => panic!("expected a foreign epoch, found {other:?}"),
        }
        assert_eq!(std::fs::read(&foreign)?, before);
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn a_bounded_integrity_failure_is_never_ready() -> TestResult {
        let root = fresh_root("ia1-b-quick-check")?;
        let (database, _) = legacy_database(&root, "quick-check")?;
        {
            let connection = Connection::open(&database)?;
            connection.execute("CREATE TABLE readiness_probe (value text NOT NULL)", [])?;
            connection.execute(
                "INSERT INTO readiness_probe VALUES (?1)",
                ["x".repeat(1_000)],
            )?;
        }
        let mut bytes = std::fs::read(&database)?;
        const ISOLATED_PAGE: usize = 4 * 4_096;
        assert!(bytes.len() > ISOLATED_PAGE);
        bytes[ISOLATED_PAGE] ^= 0xff;
        std::fs::write(&database, &bytes)?;
        let before = std::fs::read(&database)?;
        assert_corrupt(preflight_read_only(&database));
        assert_eq!(std::fs::read(&database)?, before);
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn a_quarantine_marker_refuses_before_migration_planning() -> TestResult {
        let root = fresh_root("ia1-b-quarantine")?;
        let (database, _) = legacy_database(&root, "quarantine")?;
        let migrations = ProductionMigrations::load()?;
        apply_migrations(
            &database,
            &root.join("backups"),
            "ia1-b-test",
            migrations.as_slice().as_slice(),
        )?;
        Connection::open(&database)?.execute_batch(
            "
            INSERT OR REPLACE INTO rust_migration_run
                (id, started_ms, pending, backup)
            VALUES (1, 1, '20260811_rebuild_event_tables_family_tagged', 'test-backup');
            ",
        )?;
        let before = std::fs::read(&database)?;
        match preflight_read_only(&database) {
            StoreReadiness::QuarantinedOrCorrupt(Obstruction::Quarantine(quarantine)) => {
                assert_eq!(quarantine.pending().len(), 1);
                assert_eq!(quarantine.backup().as_str(), "test-backup");
            }
            other => panic!("expected migration quarantine, found {other:?}"),
        }
        assert_eq!(std::fs::read(&database)?, before);
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn fixed_clock_successive_migrations_disambiguate_backup_filenames() -> TestResult {
        use super::{apply_with_clock, ClockAuthority, Migration, MigrationOutcome};
        #[derive(Debug)]
        struct FixedClock(i64);
        impl ClockAuthority for FixedClock {
            fn now(&self) -> grapt_types::Timestamp {
                grapt_types::Timestamp::from_millis(self.0)
            }
        }
        struct Step(&'static str, &'static str);
        impl Migration for Step {
            fn id(&self) -> grapt_types::MigrationId {
                grapt_types::MigrationId::new(self.0)
                    .unwrap_or_else(|| panic!("invalid migration id: {}", self.0))
            }
            fn up(&self, tx: &rusqlite::Transaction<'_>) -> Result<(), String> {
                tx.execute_batch(self.1).map_err(|error| error.to_string())
            }
        }

        let root = fresh_root("fixed-clock-backup-suffix")?;
        let (database, _) = legacy_database(&root, "two-steps")?;
        let backups = root.join("backups");
        let clock = FixedClock(1_700_000_000_000);
        let first = Step("20260801_first_step", "CREATE TABLE t_first (id INTEGER);");
        let second = Step(
            "20260802_second_step",
            "CREATE TABLE t_second (id INTEGER);",
        );

        let outcome_one = apply_with_clock(&database, &backups, "test-1", &[&first], &clock)?;
        let backup_one = match outcome_one {
            MigrationOutcome::Applied { backup, .. } => backup.backup,
            other => panic!("expected first migration applied, got {other:?}"),
        };
        let outcome_two =
            apply_with_clock(&database, &backups, "test-2", &[&first, &second], &clock)?;
        let backup_two = match outcome_two {
            MigrationOutcome::Applied { backup, .. } => backup.backup,
            other => panic!("expected second migration applied, got {other:?}"),
        };
        assert_ne!(backup_one, backup_two);
        assert!(backup_one.is_file() && backup_two.is_file());
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn concurrent_fixed_clock_backups_claim_distinct_intact_files() -> TestResult {
        use super::{apply_with_clock, ClockAuthority, Migration, MigrationOutcome};
        use std::collections::BTreeSet;
        use std::sync::{Arc, Barrier};

        #[derive(Debug)]
        struct FixedClock(i64);
        impl ClockAuthority for FixedClock {
            fn now(&self) -> grapt_types::Timestamp {
                grapt_types::Timestamp::from_millis(self.0)
            }
        }
        struct NoopStep;
        impl Migration for NoopStep {
            fn id(&self) -> grapt_types::MigrationId {
                grapt_types::MigrationId::new("20260801_concurrent_step")
                    .unwrap_or_else(|| panic!("invalid migration id"))
            }
            fn up(&self, tx: &rusqlite::Transaction<'_>) -> Result<(), String> {
                tx.execute_batch("CREATE TABLE t_concurrent (id INTEGER);")
                    .map_err(|error| error.to_string())
            }
        }

        let root = fresh_root("concurrent-fixed-clock-backups")?;
        let backups = Arc::new(root.join("backups"));
        let threads = 8_usize;
        let mut dbs = Vec::with_capacity(threads);
        for index in 0..threads {
            let dir = root.join(format!("db-{index}"));
            std::fs::create_dir_all(&dir)?;
            let (db_path, _) = legacy_database(&dir, "shared-name")?;
            let conn = Connection::open(&db_path)?;
            conn.execute_batch(&format!(
                "CREATE TABLE marker (val INTEGER); INSERT INTO marker (val) VALUES ({index});"
            ))?;
            dbs.push((index as i64, db_path));
        }

        let barrier = Arc::new(Barrier::new(threads));
        let mut handles = Vec::with_capacity(threads);
        for (marker, db_path) in dbs {
            let barrier = Arc::clone(&barrier);
            let backups = Arc::clone(&backups);
            handles.push(std::thread::spawn(
                move || -> Result<(i64, PathBuf), String> {
                    let clock = FixedClock(1_700_000_000_000);
                    let step = NoopStep;
                    barrier.wait();
                    let outcome =
                        apply_with_clock(&db_path, &backups, "concurrent", &[&step], &clock)
                            .map_err(|error| error.to_string())?;
                    match outcome {
                        MigrationOutcome::Applied { backup, .. } => Ok((marker, backup.backup)),
                        other => Err(format!("expected Applied, got {other:?}")),
                    }
                },
            ));
        }

        let mut seen_paths = BTreeSet::new();
        for handle in handles {
            let (expected_marker, backup_path) = handle
                .join()
                .map_err(|_| "backup thread panicked".to_owned())??;
            assert!(
                seen_paths.insert(backup_path.clone()),
                "duplicate backup path claimed: {}",
                backup_path.display()
            );
            let conn = Connection::open(&backup_path)?;
            let actual: i64 = conn.query_row("SELECT val FROM marker", [], |row| row.get(0))?;
            assert_eq!(actual, expected_marker);
        }
        assert_eq!(seen_paths.len(), threads);
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }
}
