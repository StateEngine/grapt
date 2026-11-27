//! PRODUCT-STARTUP-FSYNC-01: this service creates its schema in ONE durable
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
fn the_shell_schema_is_created_in_one_commit_and_kept_in_none() -> TestResult {
    let root =
        std::env::temp_dir().join(format!("grapt-shell-schema-commit-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&root)?;
    let database = root.join("store.db");
    let mut store = EventStore::open(&database, "0.1.0")?;
    // This counter measures rollback-journal commits, not writes retained in WAL.
    // Production's WAL policy has separate witnesses; keep this schema-atomicity probe explicit.
    store
        .connection()
        .execute_batch("PRAGMA journal_mode = DELETE")?;
    let fresh = change_counter(&database)?;
    grapt_shell::schema::initialize(&mut store)?;
    assert_eq!(
        change_counter(&database)?,
        fresh + 1,
        "PSF103 the shell created its schema in more than one durable commit"
    );
    grapt_shell::schema::initialize(&mut store)?;
    assert_eq!(
        change_counter(&database)?,
        fresh + 1,
        "a current shell schema committed again"
    );
    drop(store);
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

/// A fresh store with the shell schema, in its own directory.
fn shell_store(label: &str) -> TestResult<(std::path::PathBuf, EventStore)> {
    let root =
        std::env::temp_dir().join(format!("grapt-shell-schema-{label}-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&root)?;
    let mut store = EventStore::open(&root.join("store.db"), "0.1.0")?;
    grapt_shell::schema::initialize(&mut store)?;
    Ok((root, store))
}

/// Runs `initialize` while another connection holds the write lock, with no
/// busy wait: it succeeds only if it needed no write lock at all.
fn initialize_under_a_held_write_lock(
    root: &std::path::Path,
    store: &mut EventStore,
) -> TestResult<Result<(), grapt_shell::schema::SchemaError>> {
    let writer = rusqlite::Connection::open(root.join("store.db"))?;
    writer.execute_batch("BEGIN IMMEDIATE")?;
    store.connection().busy_timeout(std::time::Duration::ZERO)?;
    let result = grapt_shell::schema::initialize(store);
    writer.execute_batch("ROLLBACK")?;
    Ok(result)
}

/// HYGIENE-FOLLOWUPS-01 L1: an already-current shell schema is checked
/// read-only and takes no write lock, so it cannot contend with a writer.
#[test]
fn a_current_shell_schema_takes_no_write_lock() -> TestResult {
    let (root, mut store) = shell_store("no-lock")?;
    let result = initialize_under_a_held_write_lock(&root, &mut store)?;
    assert!(
        result.is_ok(),
        "HYG102 an already-current shell schema took a write lock: {result:?}"
    );
    drop(store);
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

/// The read-only check covers all three shell tables and the migrated
/// `time_started` column: with any one missing, initialize needs the lock.
#[test]
fn every_shell_object_is_part_of_the_current_check() -> TestResult {
    for (index, removal) in [
        "DROP TABLE shell_operation",
        "DROP TABLE shell_artifact",
        "DROP TABLE product_shell_binding",
        "ALTER TABLE shell_operation DROP COLUMN time_started",
    ]
    .iter()
    .enumerate()
    {
        let (root, mut store) = shell_store(&format!("drift-{index}"))?;
        rusqlite::Connection::open(root.join("store.db"))?.execute_batch(removal)?;
        let result = initialize_under_a_held_write_lock(&root, &mut store)?;
        assert!(
            result.is_err(),
            "the current check missed `{removal}`: initialize took no write lock"
        );
        drop(store);
        std::fs::remove_dir_all(&root)?;
    }
    Ok(())
}

#[test]
fn retained_shell_rows_survive_schema_initialization_and_reopen() -> TestResult {
    for legacy in [false, true] {
        let (root, mut store) = shell_store(&format!("retained-{legacy}"))?;
        if legacy {
            rusqlite::Connection::open(root.join("store.db"))?
                .execute_batch("ALTER TABLE shell_operation DROP COLUMN time_started")?;
        }
        let tx = store.governed_transaction()?;
        tx.execute_batch(
            r#"INSERT INTO shell_artifact VALUES ('artifact', 8, 3, 0, X'00ff41');
             INSERT INTO shell_operation
               (operation_id, route_id, effective_graph_id, command, cwd, shell,
                artifact_id, status, generation, pid, pipe, cleanup, time_created, time_updated)
             VALUES ('operation', 'route', 'graph', 'printf data', '/project', 'sh',
                'artifact', 'running', 7, 42, 'open', 'pending', 11, 12);
             INSERT INTO product_shell_binding
             VALUES ('operation', 'shell', '/capture', '{"sessionID":"session"}');"#,
        )?;
        tx.commit()?.discard();
        let before = retained_rows(store.connection())?;
        grapt_shell::schema::initialize(&mut store)?;
        assert_eq!(retained_rows(store.connection())?, before);
        let started: Option<i64> = store.connection().query_row(
            "SELECT time_started FROM shell_operation",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(started, None);
        drop(store);
        let mut reopened = EventStore::open(&root.join("store.db"), "0.1.0")?;
        grapt_shell::schema::initialize(&mut reopened)?;
        assert_eq!(retained_rows(reopened.connection())?, before);
        drop(reopened);
        std::fs::remove_dir_all(root)?;
    }
    Ok(())
}

fn retained_rows(conn: &rusqlite::Connection) -> TestResult<Vec<Vec<rusqlite::types::Value>>> {
    let mut rows = Vec::new();
    for query in [
        "SELECT operation_id, route_id, effective_graph_id, command, cwd, shell,
          timeout_ms, artifact_id, status, generation, pid, exit_code, error,
          evidence, pipe, cleanup, timeout_deadline, time_created, time_updated
         FROM shell_operation",
        "SELECT * FROM shell_artifact",
        "SELECT * FROM product_shell_binding",
    ] {
        let mut statement = conn.prepare(query)?;
        let columns = statement.column_count();
        let result =
            statement.query_map([], |row| (0..columns).map(|index| row.get(index)).collect())?;
        rows.extend(result.collect::<Result<Vec<Vec<rusqlite::types::Value>>, _>>()?);
    }
    Ok(rows)
}
