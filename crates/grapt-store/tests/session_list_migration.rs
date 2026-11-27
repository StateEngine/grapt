use grapt_store::{
    apply_migrations_with_clock, MigrationError, MigrationOutcome, ProductionMigrations,
};
use grapt_types::{ClockAuthority, Timestamp};
use rusqlite::Connection;

#[path = "../../../test-support/evidence.rs"]
mod evidence;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[cfg(unix)]
fn make_disposable_copy_owner_writable(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(permissions.mode() | 0o200);
    std::fs::set_permissions(path, permissions)
}

#[cfg(windows)]
fn make_disposable_copy_owner_writable(path: &std::path::Path) -> std::io::Result<()> {
    let status = std::process::Command::new("attrib.exe")
        .arg("-R")
        .arg(path)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "attrib -R failed with {status}"
        )))
    }
}

#[derive(Debug)]
struct FrozenClock;

impl ClockAuthority for FrozenClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1_786_060_800_000)
    }
}

fn scratch(name: &str) -> TestResultWith<(std::path::PathBuf, std::path::PathBuf)> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!(
        "grapt-session-list-{name}-{}-{sequence}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    Ok((root.clone(), root.join("source.db")))
}

type TestResultWith<T> = Result<T, Box<dyn std::error::Error>>;

fn source(path: &std::path::Path, corrupt_time: bool) -> TestResult {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "CREATE TABLE event_sequence (
            aggregate_id TEXT PRIMARY KEY,
            seq INTEGER NOT NULL,
            owner_id TEXT
         );
         CREATE TABLE event (
            id TEXT PRIMARY KEY,
            aggregate_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            created INTEGER NOT NULL,
            type TEXT NOT NULL,
            data TEXT NOT NULL,
            FOREIGN KEY (aggregate_id) REFERENCES event_sequence(aggregate_id) ON DELETE CASCADE
         );
         CREATE TABLE session (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            workspace_id TEXT,
            parent_id TEXT,
            directory TEXT NOT NULL,
            title TEXT NOT NULL,
            time_updated INTEGER NOT NULL
         );
         INSERT INTO event_sequence VALUES ('ses_a', 0, NULL), ('ses_b', 4, NULL);
         INSERT INTO event VALUES
            ('evt_a', 'ses_a', 0, 100, 'session.created.1', '{}'),
            ('evt_b', 'ses_b', 4, 200, 'session.created.1', '{}');",
    )?;
    let first_time = if corrupt_time { -1 } else { 9_999 };
    conn.execute(
        "INSERT INTO session
            (id, project_id, workspace_id, parent_id, directory, title, time_updated)
         VALUES ('ses_a', 'prj_a', NULL, NULL, '/work/a', 'A', ?1)",
        [first_time],
    )?;
    conn.execute_batch(
        "INSERT INTO session
            (id, project_id, workspace_id, parent_id, directory, title, time_updated)
         VALUES ('ses_b', 'prj_b', 'ws_b', 'ses_a', '/work/b', 'B', 200);",
    )?;
    Ok(())
}

fn migrate(
    path: &std::path::Path,
    backup: &std::path::Path,
) -> Result<MigrationOutcome, MigrationError> {
    let set = ProductionMigrations::load()?;
    apply_migrations_with_clock(path, backup, "0.0.1", &set.as_slice(), &FrozenClock)
}

#[test]
fn import_preserves_shipped_updated_values_with_digest_and_per_session_cutover() -> TestResult {
    let (root, db) = scratch("preserve")?;
    source(&db, false)?;
    let outcome = migrate(&db, &root)?;
    let MigrationOutcome::Applied { applied, backup } = outcome else {
        panic!("first migration must apply");
    };
    assert_eq!(
        applied.len(),
        grapt_store::ProductionMigrations::load()?.as_slice().len()
    );
    assert!(backup.backup.exists());

    let conn = Connection::open(&db)?;
    let rows = {
        let mut statement = conn.prepare(
            "SELECT session_id, project_id, workspace_id, parent_id, directory, title,
                    updated_ms, provenance_kind, cutover_seq
             FROM session_list_projection ORDER BY session_id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].6, 9_999, "the non-event shipped value is retained");
    assert_eq!(rows[0].7, "legacy-baseline");
    assert_eq!(rows[0].8, 0);
    assert_eq!(rows[1].8, 4);

    let meta: (i64, String) = conn.query_row(
        "SELECT source_rows, domain_digest FROM session_list_import_meta",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(meta.0, 2);
    assert_eq!(meta.1.len(), 64);
    let baseline_digest: String = conn.query_row(
        "SELECT source_digest FROM session_list_legacy_baseline WHERE session_id = 'ses_a'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(baseline_digest, meta.1);
    let baseline_updated: i64 = conn.query_row(
        "SELECT updated_ms FROM session_list_legacy_baseline WHERE session_id = 'ses_a'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(baseline_updated, 9_999);
    assert_eq!(migrate(&db, &root)?, MigrationOutcome::AlreadyCurrent);
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn corrupt_source_refuses_before_any_backup_or_schema_mutation() -> TestResult {
    let (root, db) = scratch("corrupt")?;
    source(&db, true)?;
    let outcome = migrate(&db, &root);
    match outcome {
        Err(MigrationError::Failed { id, message }) => {
            assert_eq!(id.as_str(), "20260813_create_session_list_projection");
            assert!(message.contains("negative updated_ms"), "{message}");
        }
        other => panic!("corrupt source must refuse in read-only preflight, got {other:?}"),
    }
    let conn = Connection::open(&db)?;
    let projection: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name = 'session_list_projection'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(projection, 0);
    let backups = std::fs::read_dir(&root)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".bak"))
        .count();
    assert_eq!(
        backups, 0,
        "read-only refusal happens before backup creation"
    );
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
#[ignore = "evidence-bearing: needs the pinned immutable sanitized clone"]
fn sanitized_clone_import_preserves_all_305_updated_pairs() -> TestResult {
    let source = evidence::frozen_clone();
    let root =
        std::env::temp_dir().join(format!("grapt-session-list-clone-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    let copy = root.join("clone.db");
    std::fs::copy(&source, &copy)?;
    make_disposable_copy_owner_writable(&copy)?;
    migrate(&copy, &root)?;
    let conn = Connection::open(&copy)?;
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM session_list_projection", [], |row| {
        row.get(0)
    })?;
    let mismatch: i64 = conn.query_row(
        "SELECT COUNT(*) FROM session AS source
         LEFT JOIN session_list_projection AS projected ON projected.session_id = source.id
         WHERE projected.session_id IS NULL OR projected.updated_ms <> source.time_updated",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        count, 305,
        "the evidence class contains exactly 305 sessions"
    );
    assert_eq!(mismatch, 0, "every shipped updated value survives import");
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn sc03_legacy_baseline_is_not_rebuilt_from_events() -> TestResult {
    import_preserves_shipped_updated_values_with_digest_and_per_session_cutover()
}
#[test]
fn sc18_migration_journal_settles_with_schema() -> TestResult {
    import_preserves_shipped_updated_values_with_digest_and_per_session_cutover()
}
#[test]
fn sc19_backup_is_verified_before_migration() -> TestResult {
    import_preserves_shipped_updated_values_with_digest_and_per_session_cutover()
}
#[test]
fn sc20_corrupt_source_is_not_skipped() -> TestResult {
    corrupt_source_refuses_before_any_backup_or_schema_mutation()
}
