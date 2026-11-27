use crate::export::{export_domain, DomainExport, DomainExportError};
use crate::journal::{self, CheckpointEvidence, Journal, JournalError, RehearsalPhase};
use crate::snapshot::{
    create_verified_snapshot, fingerprint_file, open_read_only, require_distinct_physical_files,
    revalidate_distinct_physical_files, PhysicalFileIdentity, SnapshotError,
};
use grapt_store::migration::preflight::{Obstruction, StoreReadiness};
use grapt_store::{BoundDatabaseFile, MigrationOutcome};
use grapt_types::{ClockAuthority, DatabaseFingerprint, MigrationId};
use std::path::{Path, PathBuf};

const CLONE_FILE: &str = "migration-candidate.sqlite3";
const BACKUP_FILE: &str = "pre-mutation-rollback.sqlite3";
const ROLLBACK_FILE: &str = "rollback-restored.sqlite3";
const MAX_RECORDED_MIGRATIONS: usize = 64;
const MAX_INTERNAL_BACKUPS: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum RehearsalError {
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
    #[error(transparent)]
    Export(#[from] DomainExportError),
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error("store migration: {0}")]
    Migration(#[from] grapt_store::MigrationError),
    #[error("store open for projection rebuild: {0}")]
    Store(#[from] grapt_store::WriteError),
    #[error("bound writable database: {0}")]
    BoundDatabase(#[from] grapt_store::BoundDatabaseError),
    #[error("permission projection rebuild: {0}")]
    Permission(#[from] grapt_permission::PermissionHttpError),
    #[error("permission replay source: {0}")]
    PermissionReplaySource(String),
    #[error("rehearsal sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("{stage} domain comparison failed: expected {expected}, observed {observed}")]
    DomainMismatch {
        stage: &'static str,
        expected: String,
        observed: String,
    },
    #[error("existing checkpoint paths do not match this rehearsal invocation")]
    PathMismatch,
    #[error("the read-only source aliases a migration artifact path: {0:?}")]
    SourceArtifactAlias(PathBuf),
    #[error("standalone rollback source, backup, and destination must be distinct paths")]
    RestorePathAlias,
    #[error("source database changed after its read-only audit")]
    SourceChanged,
    #[error("migration candidate changed after its durable migrated checkpoint")]
    CandidateChanged,
    #[error("migration candidate readiness refuses rehearsal: {0}")]
    Readiness(String),
    #[error("migration journal records more than {maximum} migrations")]
    TooManyMigrations { maximum: usize },
    #[error("the store created more than {maximum} bounded internal backup attempts")]
    TooManyInternalBackups { maximum: usize },
    #[error("migration completed without reaching Ready classification: {0}")]
    NotReadyAfterMigration(String),
}

#[derive(Debug, Clone)]
pub struct RollbackReceipt {
    path: PathBuf,
    file_fingerprint: DatabaseFingerprint,
    domain_fingerprint: DatabaseFingerprint,
}

impl RollbackReceipt {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file_fingerprint(&self) -> &DatabaseFingerprint {
        &self.file_fingerprint
    }

    pub fn domain_fingerprint(&self) -> &DatabaseFingerprint {
        &self.domain_fingerprint
    }
}

#[derive(Debug, Clone)]
pub struct RehearsalReceipt {
    pub(crate) root: PathBuf,
    pub(crate) run_id: MigrationId,
    source: PathBuf,
    candidate: PathBuf,
    source_file_fingerprint: DatabaseFingerprint,
    source_domain_fingerprint: DatabaseFingerprint,
    candidate_file_fingerprint: DatabaseFingerprint,
    /// The migrated candidate's canonical domain state.
    ///
    /// Recorded because it is the only ACTIVATION BASELINE that exists. Once a
    /// transition activates this candidate the engine begins writing to it, and
    /// the sole way to tell later whether any work happened after activation is
    /// to compare the domain then against the domain proven here. Without it a
    /// rollback could only guess, and guessing would let "restoring the backup
    /// is lossless" be said over discarded work.
    ///
    /// It is measured in the same read that produced `candidate_file_fingerprint`
    /// and the comparison against the source, so it costs no extra open.
    candidate_domain_fingerprint: DatabaseFingerprint,
    rollback: RollbackReceipt,
    applied_migrations: Vec<MigrationId>,
}

impl RehearsalReceipt {
    pub fn run_id(&self) -> &MigrationId {
        &self.run_id
    }

    pub fn source(&self) -> &Path {
        &self.source
    }

    pub fn candidate(&self) -> &Path {
        &self.candidate
    }

    pub fn source_file_fingerprint(&self) -> &DatabaseFingerprint {
        &self.source_file_fingerprint
    }

    pub fn source_domain_fingerprint(&self) -> &DatabaseFingerprint {
        &self.source_domain_fingerprint
    }

    /// The migrated candidate's domain state at the moment it was proven — the
    /// baseline a later rollback compares against to find work done after
    /// activation.
    pub fn candidate_domain_fingerprint(&self) -> &DatabaseFingerprint {
        &self.candidate_domain_fingerprint
    }

    pub fn candidate_file_fingerprint(&self) -> &DatabaseFingerprint {
        &self.candidate_file_fingerprint
    }

    pub fn rollback(&self) -> &RollbackReceipt {
        &self.rollback
    }

    pub fn applied_migrations(&self) -> &[MigrationId] {
        &self.applied_migrations
    }
}

pub fn inspect_status(root: &Path) -> Result<journal::RehearsalStatus, JournalError> {
    journal::inspect(root)
}

/// The canonical domain fingerprint of a database, read WITHOUT writing to it.
///
/// Exposed because a recovery disposition has to ask what the served database
/// looks like *now*, and the operator that asks must not have to open it
/// writably to find out — observing a database in order to decide whether to
/// roll it back must not itself be a first write contact that stamps ownership.
pub fn domain_fingerprint(database: &Path) -> Result<DatabaseFingerprint, RehearsalError> {
    let connection = open_read_only(database)?;
    let export = export_domain(&connection)?;
    drop(connection);
    Ok(export.fingerprint()?)
}

/// Restore a verified rollback database beside an interrupted or partially
/// migrated disposable candidate. The candidate is never overwritten: the
/// restored database is a new recovery artifact, so an operator can inspect
/// both truths before selecting either service.
pub fn restore_backup_rehearsal(
    source: &Path,
    backup: &Path,
    destination: &Path,
) -> Result<RollbackReceipt, RehearsalError> {
    let source = std::fs::canonicalize(source).map_err(SnapshotError::from)?;
    let backup = std::fs::canonicalize(backup).map_err(SnapshotError::from)?;
    let destination = normalized_destination(destination)?;
    if source == backup || source == destination || backup == destination {
        return Err(RehearsalError::RestorePathAlias);
    }
    let source_identity = PhysicalFileIdentity::required(&source)?;
    let backup_identity = PhysicalFileIdentity::required(&backup)?;
    let destination_identity = PhysicalFileIdentity::optional(&destination)?;
    require_distinct_present(&[
        Some(&source_identity),
        Some(&backup_identity),
        destination_identity.as_ref(),
    ])?;
    let source_connection = open_read_only(&source)?;
    let source_export = export_domain(&source_connection)?;
    drop(source_connection);
    let (backup_file_fingerprint, backup_export, restored_identity) = ensure_snapshot(
        &backup,
        &destination,
        &source_export,
        "standalone rollback restore",
        true,
        true,
    )?;
    revalidate_distinct_physical_files(&[&source_identity, &backup_identity, &restored_identity])?;
    compare_domain("standalone rollback", &source_export, &backup_export)?;
    Ok(RollbackReceipt {
        path: destination,
        file_fingerprint: backup_file_fingerprint,
        domain_fingerprint: backup_export.fingerprint()?,
    })
}

pub fn rehearse(
    source: &Path,
    root: &Path,
    run_id: MigrationId,
    engine_version: &str,
    clock: &dyn ClockAuthority,
) -> Result<RehearsalReceipt, RehearsalError> {
    rehearse_with_write_hook(source, root, run_id, engine_version, clock, || Ok(()))
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn rehearse_with_write_hook(
    source: &Path,
    root: &Path,
    run_id: MigrationId,
    engine_version: &str,
    clock: &dyn ClockAuthority,
    mut before_candidate_write: impl FnMut() -> Result<(), RehearsalError>,
) -> Result<RehearsalReceipt, RehearsalError> {
    std::fs::create_dir_all(root).map_err(SnapshotError::from)?;
    let root = std::fs::canonicalize(root).map_err(SnapshotError::from)?;
    let source = std::fs::canonicalize(source).map_err(SnapshotError::from)?;
    let clone_path = root.join(CLONE_FILE);
    let backup_path = root.join(BACKUP_FILE);
    let rollback_path = root.join(ROLLBACK_FILE);
    for artifact in [&clone_path, &backup_path, &rollback_path] {
        if source == *artifact {
            return Err(RehearsalError::SourceArtifactAlias(artifact.clone()));
        }
    }
    let source_identity = PhysicalFileIdentity::required(&source)?;
    let existing_clone_identity = PhysicalFileIdentity::optional(&clone_path)?;
    let existing_backup_identity = PhysicalFileIdentity::optional(&backup_path)?;
    let existing_rollback_identity = PhysicalFileIdentity::optional(&rollback_path)?;
    require_distinct_present(&[
        Some(&source_identity),
        existing_clone_identity.as_ref(),
        existing_backup_identity.as_ref(),
        existing_rollback_identity.as_ref(),
    ])?;
    let mut journal = Journal::open(&root, run_id.clone())?;

    let (_, source_file_fingerprint) = fingerprint_file(&source)?;
    let source_connection = open_read_only(&source)?;
    let source_export = export_domain(&source_connection)?;
    drop(source_connection);
    let source_domain_fingerprint = source_export.fingerprint()?;

    let mut evidence = journal
        .last_evidence()
        .cloned()
        .unwrap_or_else(|| CheckpointEvidence {
            source_path: source.clone(),
            clone_path: clone_path.clone(),
            backup_path: backup_path.clone(),
            rollback_path: rollback_path.clone(),
            source_file_fingerprint: source_file_fingerprint.as_str().to_owned(),
            source_domain_digest: source_domain_fingerprint.as_str().to_owned(),
            ..CheckpointEvidence::default()
        });
    validate_invocation_paths(
        &evidence,
        &source,
        &clone_path,
        &backup_path,
        &rollback_path,
    )?;
    if evidence.source_file_fingerprint != source_file_fingerprint.as_str()
        || evidence.source_domain_digest != source_domain_fingerprint.as_str()
    {
        return Err(RehearsalError::SourceChanged);
    }
    let resumed_after_source_audit = journal.has(RehearsalPhase::SourceAudited);
    if !resumed_after_source_audit {
        evidence.source_file_fingerprint = source_file_fingerprint.as_str().to_owned();
        evidence.source_domain_digest = source_domain_fingerprint.as_str().to_owned();
        journal.record(RehearsalPhase::SourceAudited, evidence.clone(), clock)?;
    }

    let clone_snapshot = ensure_snapshot(
        &source,
        &clone_path,
        &source_export,
        "initial disposable clone",
        !journal.has(RehearsalPhase::CloneCreated),
        !resumed_after_source_audit || !clone_path.exists(),
    );
    let (clone_file_fingerprint, clone_export, clone_identity) = match clone_snapshot {
        Ok(snapshot) => snapshot,
        Err(RehearsalError::DomainMismatch { .. }) if journal.has(RehearsalPhase::CloneCreated) => {
            return Err(RehearsalError::CandidateChanged);
        }
        Err(error) => return Err(error),
    };
    if !journal.has(RehearsalPhase::CloneCreated) {
        evidence.clone_file_fingerprint = Some(clone_file_fingerprint.as_str().to_owned());
        evidence.clone_domain_digest = Some(clone_export.digest_hex.clone());
        journal.record(RehearsalPhase::CloneCreated, evidence.clone(), clock)?;
    }

    let (backup_file_fingerprint, backup_export, backup_identity) = ensure_snapshot(
        &clone_path,
        &backup_path,
        &source_export,
        "pre-mutation rollback backup",
        !journal.has(RehearsalPhase::BackupVerified),
        true,
    )?;
    if let Some(recorded) = &evidence.backup_file_fingerprint {
        if recorded != backup_file_fingerprint.as_str() {
            return Err(RehearsalError::SourceChanged);
        }
    }
    if !journal.has(RehearsalPhase::BackupVerified) {
        evidence.backup_file_fingerprint = Some(backup_file_fingerprint.as_str().to_owned());
        evidence.backup_domain_digest = Some(backup_export.digest_hex.clone());
        journal.record(RehearsalPhase::BackupVerified, evidence.clone(), clock)?;
    }

    revalidate_distinct_physical_files(&[&source_identity, &clone_identity, &backup_identity])?;
    let candidate_authority =
        BoundDatabaseFile::from_file(&clone_path, clone_identity.try_clone_file()?)?;
    before_candidate_write()?;
    migrate_or_reconcile(
        &candidate_authority,
        &clone_path,
        &root,
        engine_version,
        clock,
    )?;
    if !journal.has(RehearsalPhase::Migrated) {
        revalidate_distinct_physical_files(&[&source_identity, &clone_identity, &backup_identity])?;
        before_candidate_write()?;
        rebuild_permission_projections(
            &candidate_authority,
            &clone_path,
            &backup_path,
            engine_version,
        )?;
    }
    let (_, observed_source_file_fingerprint) = fingerprint_file(&source)?;
    let observed_source_connection = open_read_only(&source)?;
    let observed_source_export = export_domain(&observed_source_connection)?;
    drop(observed_source_connection);
    if observed_source_file_fingerprint != source_file_fingerprint
        || observed_source_export != source_export
    {
        return Err(RehearsalError::SourceChanged);
    }
    let (_, candidate_file_fingerprint) = fingerprint_file(&clone_path)?;
    let candidate_connection = open_read_only(&clone_path)?;
    let candidate_export = export_domain(&candidate_connection)?;
    let applied_migrations = read_applied_migrations(&candidate_connection)?;
    drop(candidate_connection);
    if journal.has(RehearsalPhase::Migrated)
        && (evidence.clone_file_fingerprint.as_deref() != Some(candidate_file_fingerprint.as_str())
            || evidence.clone_domain_digest.as_deref()
                != Some(candidate_export.digest_hex.as_str()))
    {
        return Err(RehearsalError::CandidateChanged);
    }
    if !journal.has(RehearsalPhase::Migrated) {
        evidence.clone_file_fingerprint = Some(candidate_file_fingerprint.as_str().to_owned());
        evidence.clone_domain_digest = Some(candidate_export.digest_hex.clone());
        evidence.applied_migrations = applied_migrations
            .iter()
            .map(|migration| migration.as_str().to_owned())
            .collect();
        journal.record(RehearsalPhase::Migrated, evidence.clone(), clock)?;
    }
    compare_migrated_domain(&source_export, &candidate_export)?;
    if !journal.has(RehearsalPhase::MigrationCompared) {
        journal.record(RehearsalPhase::MigrationCompared, evidence.clone(), clock)?;
    }

    revalidate_distinct_physical_files(&[&source_identity, &clone_identity, &backup_identity])?;
    let (rollback_file_fingerprint, rollback_export, rollback_identity) = ensure_snapshot(
        &backup_path,
        &rollback_path,
        &source_export,
        "rollback restore",
        !journal.has(RehearsalPhase::RollbackRestored),
        true,
    )?;
    revalidate_distinct_physical_files(&[
        &source_identity,
        &clone_identity,
        &backup_identity,
        &rollback_identity,
    ])?;
    if let Some(recorded) = &evidence.rollback_file_fingerprint {
        if recorded != rollback_file_fingerprint.as_str() {
            return Err(RehearsalError::SourceChanged);
        }
    }
    if !journal.has(RehearsalPhase::RollbackRestored) {
        evidence.rollback_file_fingerprint = Some(rollback_file_fingerprint.as_str().to_owned());
        evidence.rollback_domain_digest = Some(rollback_export.digest_hex.clone());
        journal.record(RehearsalPhase::RollbackRestored, evidence.clone(), clock)?;
    }
    compare_domain("rollback", &source_export, &rollback_export)?;
    if !journal.has(RehearsalPhase::RollbackCompared) {
        journal.record(RehearsalPhase::RollbackCompared, evidence.clone(), clock)?;
    }
    if !journal.has(RehearsalPhase::ReadyForHandoff) {
        journal.record(RehearsalPhase::ReadyForHandoff, evidence.clone(), clock)?;
    }

    Ok(RehearsalReceipt {
        root,
        run_id,
        source,
        candidate: clone_path,
        source_file_fingerprint,
        source_domain_fingerprint,
        candidate_file_fingerprint,
        candidate_domain_fingerprint: candidate_export.fingerprint()?,
        rollback: RollbackReceipt {
            path: rollback_path,
            file_fingerprint: rollback_file_fingerprint,
            domain_fingerprint: rollback_export.fingerprint()?,
        },
        applied_migrations,
    })
}

fn normalized_destination(path: &Path) -> Result<PathBuf, RehearsalError> {
    if path.exists() {
        return std::fs::canonicalize(path)
            .map_err(SnapshotError::from)
            .map_err(RehearsalError::from);
    }
    let parent = path.parent().ok_or_else(|| {
        SnapshotError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "rollback destination has no parent",
        ))
    })?;
    let parent = std::fs::canonicalize(parent).map_err(SnapshotError::from)?;
    let name = path.file_name().ok_or_else(|| {
        SnapshotError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "rollback destination has no file name",
        ))
    })?;
    Ok(parent.join(name))
}

fn ensure_snapshot(
    source: &Path,
    destination: &Path,
    expected: &DomainExport,
    stage: &'static str,
    create_if_absent: bool,
    compare_expected: bool,
) -> Result<(DatabaseFingerprint, DomainExport, PhysicalFileIdentity), RehearsalError> {
    let source_identity = PhysicalFileIdentity::required(source)?;
    let destination_identity = PhysicalFileIdentity::optional(destination)?;
    require_distinct_present(&[Some(&source_identity), destination_identity.as_ref()])?;
    if destination_identity.is_none() {
        if create_if_absent {
            let _ = create_verified_snapshot(source, destination)?;
        } else {
            return Err(SnapshotError::SourceAbsent(destination.to_path_buf()).into());
        }
    }
    let destination_identity = PhysicalFileIdentity::required(destination)?;
    revalidate_distinct_physical_files(&[&source_identity, &destination_identity])?;
    let (_, fingerprint) = fingerprint_file(destination)?;
    let connection = open_read_only(destination)?;
    let observed = export_domain(&connection)?;
    drop(connection);
    if compare_expected {
        compare_domain(stage, expected, &observed)?;
    }
    Ok((fingerprint, observed, destination_identity))
}

fn require_distinct_present(
    identities: &[Option<&PhysicalFileIdentity>],
) -> Result<(), RehearsalError> {
    for (index, first) in identities.iter().enumerate() {
        let Some(first) = first else {
            continue;
        };
        for second in identities[index + 1..].iter().flatten() {
            require_distinct_physical_files(&[first, second])?;
        }
    }
    Ok(())
}

fn compare_domain(
    stage: &'static str,
    expected: &DomainExport,
    observed: &DomainExport,
) -> Result<(), RehearsalError> {
    if expected == observed {
        return Ok(());
    }
    Err(RehearsalError::DomainMismatch {
        stage,
        expected: expected.digest_hex.clone(),
        observed: observed.digest_hex.clone(),
    })
}

pub(crate) fn compare_migrated_domain(
    expected: &DomainExport,
    observed: &DomainExport,
) -> Result<(), RehearsalError> {
    let fixed_surfaces_match = expected.schema == observed.schema
        && expected.events == observed.events
        && expected.event_sequences == observed.event_sequences
        && expected.non_legacy_events == observed.non_legacy_events
        && expected.adjunct.len() == observed.adjunct.len();
    let adjunct_matches = expected
        .adjunct
        .iter()
        .zip(&observed.adjunct)
        .all(|(before, after)| {
            before == after
                || (before.table == after.table
                    && grapt_store::export::EVENT_REPLAYABLE_TABLES
                        .contains(&before.table.as_str()))
                || allowed_empty_plugin_state_creation(before, after)
        });
    if fixed_surfaces_match && adjunct_matches {
        return Ok(());
    }
    Err(RehearsalError::DomainMismatch {
        stage: "post-migration",
        expected: expected.digest_hex.clone(),
        observed: observed.digest_hex.clone(),
    })
}

/// PLUGIN-COMPATIBILITY-01 may create exactly these tables, empty, where the
/// source database lacks them. A table the source already has, populated or
/// not, must match exactly.
fn allowed_empty_plugin_state_creation(
    before: &crate::export::TableStateDigest,
    after: &crate::export::TableStateDigest,
) -> bool {
    before.table == after.table
        && matches!(
            before.table.as_str(),
            "credential" | "plugin_credential_owner" | "kv"
        )
        && before.absent
        && !after.absent
        && after.rows == 0
}

fn rebuild_permission_projections(
    candidate: &BoundDatabaseFile,
    _candidate_path: &Path,
    replay_source: &Path,
    engine_version: &str,
) -> Result<(), RehearsalError> {
    let log = grapt_store::EventLog::open_read_only(replay_source)
        .map_err(|error| RehearsalError::PermissionReplaySource(error.to_string()))?;
    let mut store = candidate.open_store(engine_version)?;
    let _ = grapt_permission::PermissionHttpState::rebuild_from_event_log(&mut store, &log)?;
    Ok(())
}

fn migrate_or_reconcile(
    candidate: &BoundDatabaseFile,
    path: &Path,
    backup_root: &Path,
    engine_version: &str,
    clock: &dyn ClockAuthority,
) -> Result<(), RehearsalError> {
    let should_migrate = match grapt_store::preflight_read_only(path) {
        StoreReadiness::Ready => false,
        StoreReadiness::MigrationNeeded(_) => true,
        StoreReadiness::QuarantinedOrCorrupt(Obstruction::Quarantine(_)) => true,
        other => return Err(RehearsalError::Readiness(format!("{other:?}"))),
    };
    if should_migrate {
        bound_internal_backups(path, backup_root)?;
        match candidate.migrate_with_clock(backup_root, engine_version, clock)? {
            MigrationOutcome::Applied { .. } | MigrationOutcome::AlreadyCurrent => {}
        }
    }
    match grapt_store::preflight_read_only(path) {
        StoreReadiness::Ready => Ok(()),
        other => Err(RehearsalError::NotReadyAfterMigration(format!("{other:?}"))),
    }
}

fn bound_internal_backups(database: &Path, root: &Path) -> Result<(), RehearsalError> {
    let stem = database
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let prefix = format!("{stem}.pre-rust-migration.");
    let mut count = 0_usize;
    for entry in std::fs::read_dir(root).map_err(SnapshotError::from)? {
        let entry = entry.map_err(SnapshotError::from)?;
        let name = entry.file_name();
        let rendered = name.to_string_lossy();
        if rendered.starts_with(&prefix) && rendered.ends_with(".bak") {
            count = count.saturating_add(1);
        }
    }
    if count >= MAX_INTERNAL_BACKUPS {
        return Err(RehearsalError::TooManyInternalBackups {
            maximum: MAX_INTERNAL_BACKUPS,
        });
    }
    Ok(())
}

fn read_applied_migrations(
    connection: &rusqlite::Connection,
) -> Result<Vec<MigrationId>, RehearsalError> {
    let mut statement = connection.prepare("SELECT id FROM rust_migration ORDER BY rowid")?;
    let mut rows = statement.query([])?;
    let mut migrations = Vec::new();
    while let Some(row) = rows.next()? {
        if migrations.len() == MAX_RECORDED_MIGRATIONS {
            return Err(RehearsalError::TooManyMigrations {
                maximum: MAX_RECORDED_MIGRATIONS,
            });
        }
        let value: String = row.get(0)?;
        let Some(migration) = MigrationId::new(value) else {
            return Err(RehearsalError::Readiness(
                "migration journal contains an empty identity".to_owned(),
            ));
        };
        migrations.push(migration);
    }
    Ok(migrations)
}

fn validate_invocation_paths(
    evidence: &CheckpointEvidence,
    source: &Path,
    clone_path: &Path,
    backup_path: &Path,
    rollback_path: &Path,
) -> Result<(), RehearsalError> {
    if evidence.source_path == source
        && evidence.clone_path == clone_path
        && evidence.backup_path == backup_path
        && evidence.rollback_path == rollback_path
    {
        Ok(())
    } else {
        Err(RehearsalError::PathMismatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_types::Timestamp;

    #[derive(Debug)]
    struct FixedClock;

    impl ClockAuthority for FixedClock {
        fn now(&self) -> Timestamp {
            Timestamp::from_millis(1_788_000_000_123)
        }
    }

    fn assert_candidate_swap_refused_at_write_boundary(swap_at_call: usize) {
        let root = std::env::temp_dir().join(format!(
            "grapt-r6mr-write-boundary-{}-{swap_at_call}",
            std::process::id()
        ));
        if root.exists() {
            std::fs::remove_dir_all(&root).unwrap_or_else(|error| {
                panic!("cannot clear exact test root {}: {error}", root.display())
            });
        }
        std::fs::create_dir_all(&root).unwrap_or_else(|error| {
            panic!("cannot create exact test root {}: {error}", root.display())
        });
        let source = root.join("source.sqlite3");
        let run_root = root.join("run");
        let connection = rusqlite::Connection::open(&source)
            .unwrap_or_else(|error| panic!("cannot create test database: {error}"));
        connection
            .execute_batch(
                "CREATE TABLE event_sequence (aggregate_id TEXT PRIMARY KEY, seq INTEGER NOT NULL, owner_id TEXT);
                 CREATE TABLE event (id TEXT PRIMARY KEY, aggregate_id TEXT NOT NULL, seq INTEGER NOT NULL, created INTEGER NOT NULL, type TEXT NOT NULL, data TEXT NOT NULL);
                 INSERT INTO event_sequence VALUES ('ses_identity', 0, NULL);
                 INSERT INTO event VALUES ('evt_identity', 'ses_identity', 0, 1, 'session.created.1', '{}');",
            )
            .unwrap_or_else(|error| panic!("cannot seed test database: {error}"));
        drop(connection);
        let before = fingerprint_file(&source)
            .unwrap_or_else(|error| panic!("cannot fingerprint test source: {error}"));
        let candidate = run_root.join(CLONE_FILE);
        let hook_source = source.clone();
        let hook_candidate = candidate.clone();
        let mut write_calls = 0_usize;
        let outcome = rehearse_with_write_hook(
            &source,
            &run_root,
            MigrationId::new("r6mr-write-boundary")
                .unwrap_or_else(|| panic!("literal migration id was refused")),
            "0.1.0",
            &FixedClock,
            || {
                write_calls = write_calls.saturating_add(1);
                if write_calls == swap_at_call {
                    std::fs::remove_file(&hook_candidate).map_err(SnapshotError::from)?;
                    std::fs::hard_link(&hook_source, &hook_candidate)
                        .map_err(SnapshotError::from)?;
                }
                Ok(())
            },
        );
        assert_eq!(
            write_calls, swap_at_call,
            "write-boundary hook was not reached"
        );
        #[cfg(unix)]
        assert!(
            matches!(
                outcome,
                Err(RehearsalError::BoundDatabase(
                    grapt_store::BoundDatabaseError::PathIdentityChanged
                ))
            ),
            "post-check swap reached an unexpected outcome: {outcome:?}"
        );
        #[cfg(windows)]
        assert!(
            matches!(
            outcome,
            Err(RehearsalError::Snapshot(SnapshotError::Io(ref error)))
                if error.kind() == std::io::ErrorKind::PermissionDenied
                    || error.raw_os_error() == Some(32)
            ),
            "Windows identity lease did not refuse replacement: {outcome:?}"
        );
        assert_eq!(
            fingerprint_file(&source)
                .unwrap_or_else(|error| panic!("cannot refingerprint test source: {error}")),
            before
        );
        std::fs::remove_dir_all(&root).unwrap_or_else(|error| {
            panic!("cannot remove exact test root {}: {error}", root.display())
        });
    }

    #[test]
    fn candidate_identity_is_revalidated_at_the_write_boundary() {
        assert_candidate_swap_refused_at_write_boundary(1);
    }

    #[test]
    fn candidate_identity_binds_projection_rebuild_write_boundary() {
        assert_candidate_swap_refused_at_write_boundary(2);
    }

    fn table(name: &str, rows: u64, digest: &str, absent: bool) -> crate::export::TableStateDigest {
        crate::export::TableStateDigest {
            table: name.to_owned(),
            rows,
            digest_hex: digest.to_owned(),
            absent,
        }
    }

    #[test]
    fn plugin_state_allowance_admits_only_new_empty_named_tables() {
        for name in ["credential", "plugin_credential_owner", "kv"] {
            assert!(
                allowed_empty_plugin_state_creation(
                    &table(name, 0, "absent", true),
                    &table(name, 0, "empty", false),
                ),
                "{name} new-empty creation was refused"
            );
            assert!(
                !allowed_empty_plugin_state_creation(
                    &table(name, 4, "populated", false),
                    &table(name, 0, "empty", false),
                ),
                "a populated {name} table could be replaced"
            );
            assert!(
                !allowed_empty_plugin_state_creation(
                    &table(name, 0, "absent", true),
                    &table(name, 1, "one", false),
                ),
                "{name} creation could smuggle rows"
            );
        }
        for name in ["project", "plugin_websearch_selection"] {
            assert!(
                !allowed_empty_plugin_state_creation(
                    &table(name, 0, "absent", true),
                    &table(name, 0, "empty", false),
                ),
                "{name} was admitted outside the ruled set"
            );
        }
    }
}
