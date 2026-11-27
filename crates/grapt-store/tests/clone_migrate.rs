//! Acceptance: COPY the real frozen clone and migrate the COPY, then open it.
//!
//! The other three `clone_*` tests are read-only audits. This one MUTATES, so
//! the safety rule is different and absolute: the frozen clone is shared
//! evidence and `AGENTS.md` forbids opening it writable. The source is opened
//! `mode=ro` for its measurements, the migration runs against a byte copy in a
//! scratch directory, and the copy is what is proved.
//!
//! Ignored in ordinary runs (visible as `ignored`, never a vacuous pass); under
//! `cargo test -p grapt-store -- --include-ignored` an absent clone FAILS. The
//! earlier early-return shape was removed across this corpus because
//! `eprintln!` in a PASSING test is captured by libtest, so the skip was
//! invisible and the suite read green with the law unwitnessed.

use grapt_store::{EventStore, MigrationOutcome};
use rusqlite::Connection;
use std::io::Write as _;
#[path = "../../../test-support/evidence.rs"]
mod evidence;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn read_only(path: &std::path::Path) -> Result<Connection, rusqlite::Error> {
    Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
}

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
            "attrib.exe -R refused disposable copy with {status}"
        )))
    }
}

#[cfg(unix)]
fn make_source_read_only(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(permissions.mode() & !0o222);
    std::fs::set_permissions(path, permissions)
}

#[cfg(windows)]
fn make_source_read_only(path: &std::path::Path) -> std::io::Result<()> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(path, permissions)
}

#[test]
fn a_read_only_source_stays_read_only_while_its_disposable_copy_becomes_writable() -> TestResult {
    let dir = std::env::temp_dir().join(format!(
        "grapt-clone-copy-permissions-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let source = dir.join("frozen.db");
    let copy = dir.join("disposable.db");
    std::fs::write(&source, b"frozen")?;
    make_source_read_only(&source)?;
    #[cfg(unix)]
    let source_mode = {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(&source)?.permissions().mode()
    };
    assert!(
        std::fs::metadata(&source)?.permissions().readonly(),
        "test precondition: governed source must be read-only"
    );

    std::fs::copy(&source, &copy)?;
    make_disposable_copy_owner_writable(&copy)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let copy_mode = std::fs::metadata(&copy)?.permissions().mode();
        assert_ne!(copy_mode & 0o200, 0, "copy owner must be able to write");
        assert_eq!(
            copy_mode & 0o077,
            source_mode & 0o077,
            "copy must not gain group or world permissions"
        );
    }
    let mut destination = std::fs::OpenOptions::new().append(true).open(&copy)?;
    destination.write_all(b"-copy")?;
    drop(destination);

    assert!(
        std::fs::metadata(&source)?.permissions().readonly(),
        "making the disposable copy writable must not change source custody"
    );
    assert_eq!(std::fs::read(&source)?, b"frozen");
    assert_eq!(std::fs::read(&copy)?, b"frozen-copy");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
#[ignore = "evidence-bearing: needs the frozen clone; run `cargo test -p grapt-store -- --include-ignored` on the evidence machine"]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn copies_the_frozen_clone_migrates_the_copy_and_opens_it() -> TestResult {
    let path = evidence::frozen_clone();

    // Measure the SOURCE read-only. These are the numbers the migrated copy
    // must still show, taken from the real corpus rather than from a fixture.
    let (events, aggregates, max_seq) = {
        let source = read_only(&path)?;
        let events: i64 = source.query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?;
        let aggregates: i64 =
            source.query_row("SELECT COUNT(*) FROM event_sequence", [], |r| r.get(0))?;
        let max_seq: i64 = source.query_row("SELECT MAX(seq) FROM event", [], |r| r.get(0))?;
        let version: i64 = source.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        assert_eq!(version, 0, "the clone is TypeScript-era");
        (events, aggregates, max_seq)
    };
    assert_eq!(events, 69_740, "D1 counted 69,740 events");
    assert_eq!(aggregates, 305, "across 305 aggregates");

    let dir = std::env::temp_dir().join(format!("grapt-f3b4c-clone-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let copy = dir.join("clone-copy.db");
    std::fs::copy(&path, &copy)?;
    make_disposable_copy_owner_writable(&copy)?;

    let outcome = EventStore::migrate(&copy, &dir, "0.0.1")?;
    let MigrationOutcome::Applied { applied, backup } = outcome else {
        let _ = std::fs::remove_dir_all(&dir);
        return Err("expected Applied on a TypeScript-era clone".into());
    };
    assert_eq!(
        applied.len(),
        grapt_store::ProductionMigrations::load()?.as_slice().len()
    );
    assert!(backup.bytes > 0, "a verified rollback point of the copy");

    // The migrated copy must satisfy the REAL open, not merely have returned
    // `Applied`: this is what runs verify_columns, the four-arm target-shape
    // probe and the index column-shape verification over 69,740 real rows.
    let store = EventStore::open(&copy, "0.0.1")?;
    let conn = store.connection();

    let migrated_events: i64 = conn.query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?;
    let migrated_aggregates: i64 =
        conn.query_row("SELECT COUNT(*) FROM event_sequence", [], |r| r.get(0))?;
    let migrated_max: i64 = conn.query_row("SELECT MAX(seq) FROM event", [], |r| r.get(0))?;
    assert_eq!(
        (migrated_events, migrated_aggregates, migrated_max),
        (events, aggregates, max_seq),
        "every event, aggregate and sequence position survives the rebuild"
    );

    let sessions: i64 = conn.query_row(
        "SELECT COUNT(*) FROM event WHERE aggregate_family = 'session'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(
        sessions, events,
        "all 69,740 legacy events are back-filled to the session family"
    );
    let unfenced: i64 = conn.query_row(
        "SELECT COUNT(*) FROM event WHERE writer_epoch IS NULL",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(
        unfenced, events,
        "the whole imported corpus stays LegacyUnfenced, with its original bytes and hashes"
    );
    let orphans: i64 = conn.query_row(
        "SELECT COUNT(*) FROM event e WHERE NOT EXISTS (
             SELECT 1 FROM event_sequence s
             WHERE s.aggregate_family = e.aggregate_family AND s.aggregate_id = e.aggregate_id)",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(
        orphans, 0,
        "the composite foreign key resolves for every row"
    );
    let violations: Vec<String> = conn
        .prepare("PRAGMA foreign_key_check")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    assert!(
        violations.is_empty(),
        "post-migration foreign key check: {violations:?}"
    );

    // The whole point of the rebuild for indexes: the source carries both
    // target names over family-BLIND columns, and `CREATE ... IF NOT EXISTS`
    // would have honoured them silently.
    let seq_columns: Vec<String> = conn
        .prepare("PRAGMA index_info(\"event_aggregate_seq_idx\")")?
        .query_map([], |r| r.get::<_, String>(2))?
        .collect::<Result<_, _>>()?;
    assert_eq!(
        seq_columns,
        vec![
            "aggregate_family".to_owned(),
            "aggregate_id".to_owned(),
            "seq".to_owned()
        ],
        "the family-blind UNIQUE index did not survive the rebuild"
    );

    // Gaps: the real corpus is monotonic-with-gaps, and a migration that
    // renumbered would be undetectable from counts alone.
    let gapped: i64 = conn.query_row(
        "SELECT COUNT(*) FROM (
             SELECT aggregate_id FROM event GROUP BY aggregate_id
             HAVING MAX(seq) + 1 <> COUNT(*))",
        [],
        |r| r.get(0),
    )?;
    assert!(
        gapped > 0,
        "the corpus is expected to carry gaps; a run where none survive is not evidence"
    );

    // The audit path agrees with the migrated file, so the rebuild preserved
    // what the strict decoder reads, not merely what COUNT(*) sees.
    let report = grapt_store::audit(&copy)?;
    assert_eq!(report.events, 69_740);
    assert_eq!(report.explicit_null_events, 0);
    assert!(report.replay_preconditions_hold());

    eprintln!(
        "migrated copy: {migrated_events} events / {migrated_aggregates} aggregates, \
         {gapped} gap-carrying aggregates, backup {} bytes",
        backup.bytes
    );
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
