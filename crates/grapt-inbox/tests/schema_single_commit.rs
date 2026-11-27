//! This service creates its schema in ONE durable
//! commit, and a current schema costs none.
//!
//! SQLite increments the database header's file change counter (bytes 24..28)
//! once per committed write transaction in the explicit rollback-journal fixture,
//! so the counter counts the commits,
//! and with them the journal and database syncs, this initialization pays.

use grapt_store::EventStore;
use std::path::Path;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn change_counter(database: &Path) -> TestResult<u32> {
    let bytes = std::fs::read(database)?;
    let field: [u8; 4] = bytes
        .get(24..28)
        .ok_or("the database is shorter than its header")?
        .try_into()?;
    Ok(u32::from_be_bytes(field))
}

#[test]
fn the_inbox_schema_is_created_in_one_commit_and_kept_in_none() -> TestResult {
    let root =
        std::env::temp_dir().join(format!("grapt-inbox-schema-commit-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&root)?;
    let database = root.join("store.db");
    let store = EventStore::open(&database, "0.1.0")?;
    // This counter measures rollback-journal commits, not writes retained in WAL.
    // Production's WAL policy has separate witnesses; keep this schema-atomicity probe explicit.
    store
        .connection()
        .execute_batch("PRAGMA journal_mode = DELETE")?;
    let fresh = change_counter(&database)?;
    grapt_inbox::Inbox::ensure_tables(store.connection())?;
    assert_eq!(
        change_counter(&database)?,
        fresh + 1,
        "PSF102 the inbox created its schema in more than one durable commit"
    );
    grapt_inbox::Inbox::ensure_tables(store.connection())?;
    assert_eq!(
        change_counter(&database)?,
        fresh + 1,
        "a current inbox schema committed again"
    );
    drop(store);
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

/// A fresh store with the inbox schema, in its own directory.
fn inbox_store(label: &str) -> TestResult<(std::path::PathBuf, EventStore)> {
    let root =
        std::env::temp_dir().join(format!("grapt-inbox-schema-{label}-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&root)?;
    let store = EventStore::open(&root.join("store.db"), "0.1.0")?;
    grapt_inbox::Inbox::ensure_tables(store.connection())?;
    Ok((root, store))
}

/// Runs `ensure_tables` while another connection holds the write lock, with
/// no busy wait: it succeeds only if it needed no write lock at all.
fn ensure_under_a_held_write_lock(
    root: &std::path::Path,
    store: &EventStore,
) -> TestResult<Result<(), grapt_inbox::InboxError>> {
    let writer = rusqlite::Connection::open(root.join("store.db"))?;
    writer.execute_batch("BEGIN IMMEDIATE")?;
    store.connection().busy_timeout(std::time::Duration::ZERO)?;
    let result = grapt_inbox::Inbox::ensure_tables(store.connection());
    writer.execute_batch("ROLLBACK")?;
    Ok(result)
}

/// An already-current inbox schema is checked
/// read-only and takes no write lock, so it cannot contend with a writer.
#[test]
fn a_current_inbox_schema_takes_no_write_lock() -> TestResult {
    let (root, store) = inbox_store("no-lock")?;
    let result = ensure_under_a_held_write_lock(&root, &store)?;
    assert!(
        result.is_ok(),
        "HYG101 an already-current inbox schema took a write lock: {result:?}"
    );
    drop(store);
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

/// The read-only check covers every table the inbox creates and every column
/// it migrates: with any one of them missing, ensure_tables needs the write
/// lock again. The table list is read from a fresh store, not restated here.
#[test]
fn every_inbox_object_is_part_of_the_settled_check() -> TestResult {
    let bare_root =
        std::env::temp_dir().join(format!("grapt-inbox-schema-bare-{}", std::process::id()));
    if bare_root.exists() {
        std::fs::remove_dir_all(&bare_root)?;
    }
    std::fs::create_dir_all(&bare_root)?;
    let tables = |path: &std::path::Path| -> TestResult<std::collections::BTreeSet<String>> {
        let connection = rusqlite::Connection::open(path)?;
        let mut statement = connection.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )?;
        let names = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<_, _>>()?;
        Ok(names)
    };
    drop(EventStore::open(&bare_root.join("store.db"), "0.1.0")?);
    let store_tables = tables(&bare_root.join("store.db"))?;
    std::fs::remove_dir_all(&bare_root)?;

    let (probe_root, probe) = inbox_store("objects")?;
    let inbox_tables: Vec<String> = tables(&probe_root.join("store.db"))?
        .difference(&store_tables)
        .cloned()
        .collect();
    drop(probe);
    std::fs::remove_dir_all(&probe_root)?;
    assert!(!inbox_tables.is_empty(), "the inbox created no table");

    let mut removals: Vec<String> = inbox_tables
        .iter()
        .map(|table| format!("DROP TABLE {table}"))
        .collect();
    for (table, column) in [
        ("session_pending", "input_type"),
        ("session_pending", "description"),
        ("session_pending", "metadata_json"),
        ("session_message", "description"),
        ("session_message", "metadata_json"),
    ] {
        removals.push(format!("ALTER TABLE {table} DROP COLUMN {column}"));
    }
    for (index, removal) in removals.iter().enumerate() {
        let (root, store) = inbox_store(&format!("drift-{index}"))?;
        rusqlite::Connection::open(root.join("store.db"))?.execute_batch(removal)?;
        let result = ensure_under_a_held_write_lock(&root, &store)?;
        assert!(
            result.is_err(),
            "the settled check missed `{removal}`: ensure_tables took no write lock"
        );
        drop(store);
        std::fs::remove_dir_all(&root)?;
    }
    Ok(())
}

/// The read-only check also guards the legacy synthetic-input migration: on a
/// store whose schema is current, (a) an unmigrated legacy synthetic admission
/// and (b) a pending row for an already-audited input must still take the
/// write path and migrate.
#[test]
fn a_current_inbox_schema_still_migrates_legacy_synthetic_inputs() -> TestResult {
    let pending = |connection: &rusqlite::Connection, id: &str| -> TestResult<i64> {
        Ok(connection.query_row(
            "SELECT COUNT(*) FROM session_pending WHERE id = ?1",
            [id],
            |row| row.get(0),
        )?)
    };

    // (a) A legacy synthetic admission whose input is not yet audited.
    let (root, store) = inbox_store("legacy-synthetic")?;
    let raw = rusqlite::Connection::open(root.join("store.db"))?;
    raw.execute_batch(
        "INSERT INTO event_sequence (aggregate_family, aggregate_id, seq, owner_id)
             VALUES ('session', 'ses_legacy', 0, NULL);
         INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
             VALUES ('evt_legacy', 'session', 'ses_legacy', 0, 1, 'session.input.admitted.1',
                     '{\"inputID\":\"inp_legacy\",\"sessionID\":\"ses_legacy\",\"input\":{\"type\":\"synthetic\"}}', 1);
         INSERT INTO session_pending (id, session_id, delivery, text, admitted_seq)
             VALUES ('inp_legacy', 'ses_legacy', 'queue', 'legacy', 0);",
    )?;
    grapt_inbox::Inbox::ensure_tables(store.connection())?;
    let audited: i64 = raw.query_row(
        "SELECT COUNT(*) FROM inbox_legacy_synthetic_audit WHERE input_id = 'inp_legacy'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        (audited, pending(&raw, "inp_legacy")?),
        (1, 0),
        "HYG106 a current schema skipped an unmigrated legacy synthetic admission"
    );
    drop(raw);
    drop(store);
    std::fs::remove_dir_all(&root)?;

    // (b) A pending row whose input is already in the audit.
    let (root, store) = inbox_store("audited-pending")?;
    let raw = rusqlite::Connection::open(root.join("store.db"))?;
    raw.execute_batch(
        "INSERT INTO inbox_legacy_synthetic_audit (input_id, session_id, event_data)
             VALUES ('inp_audited', 'ses_audited', '{}');
         INSERT INTO session_pending (id, session_id, delivery, text, admitted_seq)
             VALUES ('inp_audited', 'ses_audited', 'queue', 'audited', 0);",
    )?;
    grapt_inbox::Inbox::ensure_tables(store.connection())?;
    assert_eq!(
        pending(&raw, "inp_audited")?,
        0,
        "HYG106 a current schema kept a pending row for an audited synthetic input"
    );
    drop(raw);
    drop(store);
    std::fs::remove_dir_all(&root)?;
    Ok(())
}
