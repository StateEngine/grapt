use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn mid(raw: &str) -> MigrationId {
    match MigrationId::new(raw) {
        Some(i) => i,
        None => panic!("empty migration id"),
    }
}

#[test]
fn sc19_altered_backup_is_refused_by_verification() -> TestResult {
    let root = std::env::temp_dir().join(format!("grapt-altered-backup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    let path = root.join("altered.db");
    std::fs::write(&path, b"not a sqlite database")?;
    assert!(verify_backup(&path).is_err());
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

struct AddTable;
impl Migration for AddTable {
    fn id(&self) -> MigrationId {
        mid("20260808_add_widget")
    }
    fn up(&self, tx: &rusqlite::Transaction<'_>) -> Result<(), String> {
        tx.execute_batch("CREATE TABLE widget (id TEXT PRIMARY KEY)")
            .map_err(|e| e.to_string())
    }
}
struct AddSecond;
impl Migration for AddSecond {
    fn id(&self) -> MigrationId {
        mid("20260808_add_second")
    }
    fn up(&self, tx: &rusqlite::Transaction<'_>) -> Result<(), String> {
        tx.execute_batch("CREATE TABLE second (id TEXT PRIMARY KEY)")
            .map_err(|e| e.to_string())
    }
}
struct Boom;
impl Migration for Boom {
    fn id(&self) -> MigrationId {
        mid("20260808_boom")
    }
    fn up(&self, _tx: &rusqlite::Transaction<'_>) -> Result<(), String> {
        Err("boom".into())
    }
}

fn scratch(name: &str) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("grapt-mig-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("scratch.db");
    let conn = Connection::open(&db)?;
    conn.execute_batch(
        "CREATE TABLE session (id TEXT PRIMARY KEY); INSERT INTO session VALUES ('ses_m1')",
    )?;
    Ok((dir, db))
}

fn user_version(db: &Path) -> Result<u32, Box<dyn std::error::Error>> {
    Ok(Connection::open(db)?.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

#[test]
fn applies_behind_a_unique_verified_backup_then_is_idempotent() -> TestResult {
    let (dir, db) = scratch("ok")?;
    let out = apply(&db, &dir, "0.0.1", &[&AddTable])?;
    let MigrationOutcome::Applied { applied, backup } = out else {
        panic!("expected Applied");
    };
    assert_eq!(applied, vec![AddTable.id()]);
    assert!(backup.backup.exists());
    assert!(backup.bytes > 0);
    assert_eq!(user_version(&db)?, 2, "first txn stamped the epoch");
    assert_eq!(
        apply(&db, &dir, "0.0.1", &[&AddTable])?,
        MigrationOutcome::AlreadyCurrent
    );

    // A later migration takes a NEW rollback point; the first is preserved.
    let out2 = apply(&db, &dir, "0.0.1", &[&AddTable, &AddSecond])?;
    let MigrationOutcome::Applied {
        backup: backup2, ..
    } = out2
    else {
        panic!("expected Applied");
    };
    assert_ne!(
        backup2.backup, backup.backup,
        "previous rollback point never overwritten"
    );
    assert!(backup.backup.exists());
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn failed_first_migration_leaves_epoch_schema_and_journal_untouched() -> TestResult {
    let (dir, db) = scratch("fail")?;
    assert!(matches!(
        apply(&db, &dir, "0.0.1", &[&Boom]),
        Err(MigrationError::Failed { .. })
    ));
    assert_eq!(user_version(&db)?, 0, "no epoch stamp survives the failure");
    let conn = Connection::open(&db)?;
    let objects: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('rust_migration', 'engine_epoch')",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(objects, 0, "no journal or attribution left behind");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn concurrent_commit_after_backup_refuses_without_mutation() -> TestResult {
    let (dir, db) = scratch("stale")?;
    let db_for_hook = db.clone();
    let result = apply_inner(
        &db,
        &dir,
        "0.0.1",
        &[&AddTable],
        &crate::clock::SystemClock,
        Interrupt::AfterBackup(Box::new(move || {
            // A foreign connection commits between backup and the first
            // migration transaction: the snapshot no longer matches.
            let intruder = match Connection::open(&db_for_hook) {
                Ok(c) => c,
                Err(e) => panic!("intruder open: {e}"),
            };
            if let Err(e) = intruder.execute_batch("INSERT INTO session VALUES ('ses_intruder')") {
                panic!("intruder commit: {e}");
            }
        })),
    );
    assert!(matches!(result, Err(MigrationError::SnapshotStale)));
    // The stale backup file was removed: no apparent rollback point survives.
    let leftover_backups = std::fs::read_dir(&dir)?
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".bak"))
        .count();
    assert_eq!(
        leftover_backups, 0,
        "stale backup must not survive as a rollback point"
    );
    assert_eq!(user_version(&db)?, 0, "refusal performed no mutation");
    let conn = Connection::open(&db)?;
    let objects: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('rust_migration', 'engine_epoch', 'widget')",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(
        objects, 0,
        "no journal/attribution/migration schema left behind"
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

// ---------------------------------------------------------------------
// The crash-cut grid.
//
// Every cut below is a REAL interrupted run through `apply_inner`, not a
// hand-written marker row: the seam stops the run and the durable state is
// then read back from a fresh connection. What each one asserts is WHAT THE
// NEXT OPEN DOES, because that is the only thing an operator experiences.
// ---------------------------------------------------------------------

fn quarantine_of(db: &Path) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let conn = Connection::open(db)?;
    Ok(crate::quarantine::inspect(&conn)?.map(|q| {
        q.pending()
            .iter()
            .map(MigrationId::as_str)
            .collect::<Vec<_>>()
            .join(",")
    }))
}

fn journalled(db: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let conn = Connection::open(db)?;
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='rust_migration'",
        [],
        |r| r.get(0),
    )?;
    if exists == 0 {
        return Ok(Vec::new());
    }
    let mut statement = conn.prepare("SELECT id FROM rust_migration ORDER BY id")?;
    let rows = statement.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn cut(
    db: &Path,
    dir: &Path,
    interrupt: Interrupt,
) -> Result<MigrationError, Box<dyn std::error::Error>> {
    match apply_inner(
        db,
        dir,
        "0.0.1",
        &[&AddTable, &AddSecond],
        &crate::clock::SystemClock,
        interrupt,
    ) {
        Ok(outcome) => panic!("expected an interruption, got {outcome:?}"),
        Err(error) => Ok(error),
    }
}

#[test]
fn a_cut_before_the_first_transaction_leaves_the_database_unquarantined_and_untouched() -> TestResult
{
    let (dir, db) = scratch("cut-before-first")?;
    let error = cut(&db, &dir, Interrupt::BeforeFirstTransaction)?;
    assert!(matches!(
        error,
        MigrationError::Interrupted("before-first-transaction")
    ));
    assert_eq!(user_version(&db)?, 0, "no stamp survives the cut");
    assert_eq!(journalled(&db)?, Vec::<String>::new(), "no journal");
    assert_eq!(
        quarantine_of(&db)?,
        None,
        "nothing landed, so nothing is half-migrated and the marker must NOT exist"
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn a_cut_before_a_commit_rolls_back_marker_journal_and_schema_together() -> TestResult {
    let (dir, db) = scratch("cut-before-commit")?;
    let error = cut(&db, &dir, Interrupt::BeforeCommitOf(0))?;
    assert!(matches!(
        error,
        MigrationError::Interrupted("before-commit")
    ));
    assert_eq!(user_version(&db)?, 0, "the stamp rolled back with the rest");
    assert_eq!(journalled(&db)?, Vec::<String>::new());
    assert_eq!(
        quarantine_of(&db)?,
        None,
        "the marker is written in the SAME transaction as the work it covers, so it \
         rolls back with it: an interruption before a commit leaves nothing to resume"
    );
    let conn = Connection::open(&db)?;
    let widget: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name = 'widget'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(widget, 0, "the migration's own schema change rolled back");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn a_cut_between_migrations_quarantines_the_database_and_the_next_open_refuses() -> TestResult {
    let (dir, db) = scratch("cut-between")?;
    let error = cut(&db, &dir, Interrupt::AfterCommitOf(0))?;
    assert!(matches!(error, MigrationError::Interrupted("after-commit")));
    assert_eq!(
        journalled(&db)?,
        vec!["20260808_add_widget".to_owned()],
        "the first migration is durable"
    );
    assert_eq!(
        quarantine_of(&db)?.as_deref(),
        Some("20260808_add_second"),
        "the marker names what is STILL owed, not what the run set out to do"
    );

    // THE POINT. Without the marker this database opens: its shape is
    // whatever the completed migrations left, and `verify_columns` plus the
    // target-shape probe can be satisfied by accident.
    match crate::write::EventStore::open(&db, "0.0.1") {
        Err(crate::write::WriteError::MigrationQuarantined { detail }) => {
            assert!(
                detail.contains("20260808_add_second"),
                "the refusal names the outstanding migration: {detail}"
            );
            assert!(
                detail.contains(".bak"),
                "and points at the verified rollback point: {detail}"
            );
        }
        Err(other) => panic!("expected MigrationQuarantined, got {other}"),
        Ok(_) => panic!(
            "a half-migrated database OPENED successfully: the interrupted run left no \n                 refusal, so an operator would have written into partially converted data"
        ),
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn a_completed_run_leaves_no_marker_and_the_database_opens() -> TestResult {
    let (dir, db) = scratch("cut-complete")?;
    apply(&db, &dir, "0.0.1", &[&AddTable, &AddSecond])?;
    assert_eq!(
        quarantine_of(&db)?,
        None,
        "the marker is cleared in the SAME transaction as the last migration"
    );
    // The scope inversion for the refusal above: the identical sequence,
    // run to completion, opens. So the refusal is attributable to the
    // unfinished run and not to anything else these migrations did.
    crate::write::EventStore::open(&db, "0.0.1")?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn a_quarantined_database_resumes_and_completes() -> TestResult {
    let (dir, db) = scratch("resume")?;
    cut(&db, &dir, Interrupt::AfterCommitOf(0))?;
    assert!(quarantine_of(&db)?.is_some());

    // Resume is re-running the set. Quarantine authorizes detection,
    // refusal and resumption — never an automatic re-drive: nothing here
    // repairs the database or replays the migration that already landed.
    let outcome = apply(&db, &dir, "0.0.1", &[&AddTable, &AddSecond])?;
    let MigrationOutcome::Applied { applied, .. } = outcome else {
        panic!("expected Applied on resume, got {outcome:?}");
    };
    assert_eq!(
        applied,
        vec![AddSecond.id()],
        "only the outstanding migration ran; the recorded one was not replayed"
    );
    assert_eq!(quarantine_of(&db)?, None);
    crate::write::EventStore::open(&db, "0.0.1")?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn a_cut_between_rebuild_steps_has_the_same_durable_outcome_as_a_cut_before_commit() -> TestResult {
    // The rebuild is the largest single statement sequence in this crate,
    // so "interrupted half way through the rebuild" deserves its own real
    // run. Its durable outcome is IDENTICAL to a cut before the commit —
    // the transaction rolls back either way — and this asserts that rather
    // than presenting two witnesses for one observable.
    let dir = std::env::temp_dir().join(format!("grapt-mig-rebuild-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("scratch.db");
    {
        let conn = Connection::open(&db)?;
        conn.execute_batch(
            "CREATE TABLE event_sequence (aggregate_id TEXT PRIMARY KEY, seq INTEGER NOT NULL, owner_id TEXT);
             CREATE TABLE event (id TEXT PRIMARY KEY, aggregate_id TEXT NOT NULL, seq INTEGER NOT NULL,
                 created INTEGER NOT NULL, type TEXT NOT NULL, data TEXT NOT NULL,
                 FOREIGN KEY (aggregate_id) REFERENCES event_sequence(aggregate_id) ON DELETE CASCADE);
             INSERT INTO event_sequence VALUES ('ses_x', 0, NULL);
             INSERT INTO event VALUES ('evt_x','ses_x',0,1,'session.created.1','{}');",
        )?;
    }
    let set = crate::migrations::ProductionMigrations::load()?;

    crate::migrations::arm_interrupt_between_rebuild_steps();
    let result = apply(&db, &dir, "0.0.1", &set.as_slice());
    crate::migrations::disarm_interrupt_between_rebuild_steps();
    assert!(
        matches!(result, Err(MigrationError::Failed { .. })),
        "the interrupted rebuild is a typed failure, got {result:?}"
    );

    assert_eq!(user_version(&db)?, 0, "no ownership stamp survives");
    assert_eq!(journalled(&db)?, Vec::<String>::new());
    assert_eq!(
        quarantine_of(&db)?,
        None,
        "same outcome as a cut before commit"
    );
    let conn = Connection::open(&db)?;
    let rows: i64 = conn.query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?;
    assert_eq!(rows, 1, "the source rows are exactly as they were");
    let rebuilt: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE '%_rebuilt'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(rebuilt, 0, "no half-built table survives the rollback");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn refuses_duplicates_and_unknown_recorded_ids_before_any_backup() -> TestResult {
    let (dir, db) = scratch("refuse")?;
    assert!(matches!(
        apply(&db, &dir, "0.0.1", &[&AddTable, &AddTable]),
        Err(MigrationError::DuplicateSupplied(_))
    ));
    // Simulate a newer engine's journal entry.
    {
        let conn = Connection::open(&db)?;
        conn.execute_batch(
            "CREATE TABLE rust_migration (id TEXT PRIMARY KEY, time_completed INTEGER NOT NULL);
             INSERT INTO rust_migration VALUES ('20990101_from_the_future', 0)",
        )?;
    }
    assert!(matches!(
        apply(&db, &dir, "0.0.1", &[&AddTable]),
        Err(MigrationError::UnknownRecorded(_))
    ));
    // No refusal path produced a backup file.
    let backups = std::fs::read_dir(&dir)?
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".bak"))
        .count();
    assert_eq!(backups, 0, "refusals happen before any backup/mutation");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
