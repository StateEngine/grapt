//! Public witnesses for the authority-bound store readiness seam.

use grapt_store::{
    preflight_read_only, CreateBackupFile, EventStore, MigrationPlan, OwnedEventStore,
    OwnedOpenAuthority, OwnedOpenError, OwnedOpenReceipt, StoreReadiness, WriteBackupFile,
};
use grapt_types::{ClockAuthority, IdAuthority, IdToken, Timestamp};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
struct FixedAuthorities;

impl ClockAuthority for FixedAuthorities {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1_786_060_800_123)
    }
}

impl IdAuthority for FixedAuthorities {
    fn mint(&self) -> IdToken {
        let Some(token) = IdToken::new("owned-open-integration") else {
            panic!("the fixed owned-open identity must be non-empty");
        };
        token
    }
}

fn scratch(label: &str) -> TestResult<PathBuf> {
    let name = format!(
        "grapt-owned-open-integration-{label}-{}",
        std::process::id()
    );
    let root = std::env::temp_dir().join(name);
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&root)?;
    Ok(root)
}

fn legacy_database(path: &Path) -> TestResult {
    Connection::open(path)?.execute_batch(
        "CREATE TABLE event_sequence (aggregate_id text PRIMARY KEY, seq integer NOT NULL, owner_id text);
        CREATE TABLE event (id text PRIMARY KEY, aggregate_id text NOT NULL, seq integer NOT NULL, created integer NOT NULL, type text NOT NULL, data text NOT NULL);
        CREATE TABLE session (id text PRIMARY KEY, project_id text NOT NULL, workspace_id text, parent_id text, directory text NOT NULL, title text NOT NULL, time_updated integer NOT NULL);
        INSERT INTO event_sequence VALUES ('ses_owned_open', 0, NULL);
        INSERT INTO event VALUES ('evt_owned_open', 'ses_owned_open', 0, 1, 'session.created.1', '{}');
        INSERT INTO session VALUES ('ses_owned_open', 'project', NULL, NULL, 'E:/w', 'Owned open', 1);",
    )?;
    Ok(())
}

fn migration_plan(path: &Path) -> MigrationPlan {
    match preflight_read_only(path) {
        StoreReadiness::MigrationNeeded(plan) => plan,
        other => panic!("expected migration-needed readiness, found {other:?}"),
    }
}

fn open_with(
    path: &Path,
    authority: OwnedOpenAuthority,
    authorities: &FixedAuthorities,
) -> Result<OwnedEventStore, OwnedOpenError> {
    EventStore::open_owned_with_authorities(
        path,
        "owned-open-integration",
        authority,
        authorities,
        authorities,
    )
}

#[test]
fn fresh_initialization_returns_atomic_ready_store_without_backup() -> TestResult {
    let root = scratch("fresh-initialization")?;
    let database = root.join("store.db");
    let authorities = FixedAuthorities;
    let opened = open_with(&database, OwnedOpenAuthority::Initialize, &authorities)?;
    assert!(matches!(
        opened.receipt(),
        OwnedOpenReceipt::Initialized(grapt_store::MigrationOutcome::AlreadyCurrent)
    ));
    assert!(matches!(
        preflight_read_only(&database),
        StoreReadiness::Ready
    ));
    let rows: i64 =
        opened
            .store()
            .connection()
            .query_row("SELECT COUNT(*) FROM rust_migration", [], |r| r.get(0))?;
    assert_eq!(
        rows,
        i64::try_from(grapt_store::ProductionMigrations::load()?.as_slice().len())?
    );
    assert_eq!(backup_count(&root)?, 0);
    drop(opened);
    let reopened = open_with(&database, OwnedOpenAuthority::OpenReady, &authorities)?;
    assert!(matches!(reopened.receipt(), OwnedOpenReceipt::OpenedReady));
    drop(reopened);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn exact_authorized_migration_returns_a_ready_usable_governed_store() -> TestResult {
    let root = scratch("authorized-migration")?;
    let database = root.join("store.db");
    legacy_database(&database)?;
    let plan = migration_plan(&database);
    let authorities = FixedAuthorities;
    let opened = open_with(
        &database,
        OwnedOpenAuthority::Migrate(plan.clone()),
        &authorities,
    )?;
    assert!(matches!(
        opened.receipt(),
        OwnedOpenReceipt::Migrated { .. }
    ));
    assert!(matches!(
        preflight_read_only(&database),
        StoreReadiness::Ready
    ));
    let before = backup_count(&root)?;
    drop(opened);
    let opened = open_with(&database, OwnedOpenAuthority::OpenReady, &authorities)?;
    assert!(matches!(opened.receipt(), OwnedOpenReceipt::OpenedReady));
    assert_eq!(backup_count(&root)?, before);
    drop(opened);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn open_ready_authority_cannot_initialize_an_absent_path() -> TestResult {
    let root = scratch("absent-refusal")?;
    let database = root.join("store.db");
    let result = open_with(&database, OwnedOpenAuthority::OpenReady, &FixedAuthorities);
    assert!(matches!(
        result,
        Err(OwnedOpenError::AuthorityMismatch { observed, authority })
            if matches!(*observed, StoreReadiness::Absent)
                && matches!(*authority, OwnedOpenAuthority::OpenReady)
    ));
    assert!(!database.exists());
    std::fs::remove_dir_all(root)?;
    Ok(())
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
fn migration_authority_binds_pending_backup_location_and_permissions() -> TestResult {
    for mutation in 0..3 {
        let root = scratch(&format!("stale-plan-{mutation}"))?;
        let database = root.join("store.db");
        legacy_database(&database)?;
        let mut stale = migration_plan(&database);
        match mutation {
            0 => {
                stale.pending.pop();
            }
            1 => stale.backup.location = root.join("alternate-backups"),
            2 => stale.backup.permissions.create_file = CreateBackupFile::Refused,
            other => {
                return Err(std::io::Error::other(format!(
                    "unexpected stale-plan mutation ordinal {other}"
                ))
                .into());
            }
        }
        let before = std::fs::read(&database)?;
        let result = open_with(
            &database,
            OwnedOpenAuthority::Migrate(stale.clone()),
            &FixedAuthorities,
        );
        let Err(OwnedOpenError::StaleMigrationPlan {
            authorized,
            observed,
        }) = result
        else {
            panic!("expected stale-plan refusal");
        };
        assert_eq!(*authorized, stale);
        assert!(matches!(*observed, StoreReadiness::MigrationNeeded(_)));
        assert_eq!(std::fs::read(&database)?, before);
        assert_eq!(backup_count(&root)?, 0);
        std::fs::remove_dir_all(root)?;
    }
    Ok(())
}

fn assert_stale_plan_refused_without_writes(
    label: &str,
    mutate: impl FnOnce(&mut MigrationPlan),
) -> TestResult {
    let root = scratch(label)?;
    let database = root.join("store.db");
    legacy_database(&database)?;
    let mut stale = migration_plan(&database);
    mutate(&mut stale);
    let before = std::fs::read(&database)?;
    let result = open_with(
        &database,
        OwnedOpenAuthority::Migrate(stale.clone()),
        &FixedAuthorities,
    );
    let Err(OwnedOpenError::StaleMigrationPlan {
        authorized,
        observed,
    }) = result
    else {
        panic!("expected stale-plan refusal");
    };
    assert_eq!(*authorized, stale);
    assert!(matches!(*observed, StoreReadiness::MigrationNeeded(_)));
    assert_eq!(std::fs::read(&database)?, before);
    assert_eq!(backup_count(&root)?, 0);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn migration_authority_binds_minimum_free_bytes() -> TestResult {
    assert_stale_plan_refused_without_writes("stale-minimum-free-bytes", |stale| {
        stale.backup.minimum_free_bytes ^= 1;
    })
}

#[test]
fn migration_authority_binds_write_file_permission() -> TestResult {
    assert_stale_plan_refused_without_writes("stale-write-file", |stale| {
        stale.backup.permissions.write_file = WriteBackupFile::Refused;
    })
}

#[test]
fn backup_permission_revocation_stales_authority_before_database_mutation() -> TestResult {
    let root = scratch("backup-permission-revocation")?;
    let database = root.join("store.db");
    legacy_database(&database)?;
    let plan = migration_plan(&database);
    let before = std::fs::read(&database)?;
    let original_permissions = std::fs::metadata(&root)?.permissions();
    let mut revoked_permissions = original_permissions.clone();
    revoked_permissions.set_readonly(true);
    std::fs::set_permissions(&root, revoked_permissions)?;
    let result = open_with(
        &database,
        OwnedOpenAuthority::Migrate(plan),
        &FixedAuthorities,
    );
    std::fs::set_permissions(&root, original_permissions)?;
    assert!(
        matches!(result, Err(OwnedOpenError::StaleMigrationPlan { .. })),
        "revoked backup permission did not stale migration authority"
    );
    assert_eq!(std::fs::read(&database)?, before);
    assert_eq!(backup_count(&root)?, 0);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn unreadable_corrupt_and_quarantined_paths_remain_distinct_refusals() -> TestResult {
    let root = scratch("unreadable-corrupt")?;
    let directory = root.join("directory.db");
    std::fs::create_dir(&directory)?;
    let unreadable = open_with(
        &directory,
        OwnedOpenAuthority::Initialize,
        &FixedAuthorities,
    );
    assert!(matches!(
        unreadable,
        Err(OwnedOpenError::Unreadable(error))
            if matches!(*error, grapt_store::Unreadable::NotAFile)
    ));

    let corrupt = root.join("corrupt.db");
    std::fs::write(&corrupt, b"definitely not SQLite")?;
    let before = std::fs::read(&corrupt)?;
    let refused = open_with(&corrupt, OwnedOpenAuthority::Initialize, &FixedAuthorities);
    assert!(matches!(
        refused,
        Err(OwnedOpenError::QuarantinedOrCorrupt(_))
    ));
    assert_eq!(std::fs::read(&corrupt)?, before);
    let database = root.join("store.db");
    let authorities = FixedAuthorities;
    legacy_database(&database)?;
    let initialized = open_with(
        &database,
        OwnedOpenAuthority::Migrate(migration_plan(&database)),
        &authorities,
    )?;
    initialized.store().connection().execute(
        "INSERT OR REPLACE INTO rust_migration_run
            (id, started_ms, pending, backup)
         VALUES (1, 1, 'future_pending', 'verified-backup')",
        [],
    )?;
    drop(initialized);
    let before = std::fs::read(&database)?;

    let refused = open_with(&database, OwnedOpenAuthority::OpenReady, &authorities);
    assert!(matches!(
        refused,
        Err(OwnedOpenError::QuarantinedOrCorrupt(obstruction))
            if matches!(*obstruction, grapt_store::Obstruction::Quarantine(_))
    ));
    assert_eq!(std::fs::read(&database)?, before);
    std::fs::remove_dir_all(root)?;
    Ok(())
}
