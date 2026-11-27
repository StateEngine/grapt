//! Closure witnesses for the R4 session-list/keyset R1 HOLD.
//!
//! Every test here encodes a counterexample the cold review observed RED
//! against candidate `dadf4802`. They are deliberately written as end-to-end
//! store behaviour rather than white-box assertions, because both defects were
//! reachable through the ordinary production entry points: a bounded legacy
//! import that silently dropped a valid row, and a normal `EventStore::open`
//! that manufactured an empty projection beside populated legacy state.

use grapt_store::session_list::SqliteVmBudget;
use grapt_store::{EventStore, MigrationOutcome};
use rusqlite::Connection;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn scratch(
    name: &str,
) -> Result<(std::path::PathBuf, std::path::PathBuf), Box<dyn std::error::Error>> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!(
        "grapt-session-list-closure-{name}-{}-{sequence}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    Ok((root.clone(), root.join("source.db")))
}

/// Creates the fact-log tables every production open expects, plus a legacy
/// `session` table whose declared collation is supplied by the caller. The
/// collation is the whole point: the batch reader orders with an explicit
/// `COLLATE BINARY` but its continuation predicate inherited the column's
/// declared collation, so the two disagreed at the batch boundary.
fn legacy_source(path: &std::path::Path, id_collation: &str) -> TestResult {
    let conn = Connection::open(path)?;
    conn.execute_batch(&format!(
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
            id TEXT {id_collation},
            project_id TEXT NOT NULL,
            workspace_id TEXT,
            parent_id TEXT,
            directory TEXT NOT NULL,
            title TEXT NOT NULL,
            time_updated INTEGER NOT NULL
         );"
    ))?;
    Ok(())
}

/// 513 rows straddling the 512-row import batch boundary. The uppercase run
/// sorts entirely before the single lowercase row under BINARY, so the
/// lowercase row is the first row of the second batch and is exactly the row a
/// collation-disagreeing continuation predicate loses.
fn insert_boundary_rows(conn: &mut Connection) -> TestResult {
    let tx = conn.transaction()?;
    for index in 0..512 {
        tx.execute(
            "INSERT INTO session
                (id, project_id, workspace_id, parent_id, directory, title, time_updated)
             VALUES (?1, 'prj_a', NULL, NULL, '/work/a', ?2, ?3)",
            rusqlite::params![
                format!("ses_A{index:03}"),
                format!("title-{index:03}"),
                i64::from(index) + 1_000
            ],
        )?;
    }
    tx.commit()?;
    conn.execute(
        "INSERT INTO session
            (id, project_id, workspace_id, parent_id, directory, title, time_updated)
         VALUES ('ses_a000', 'prj_a', 'ws_z', NULL, '/work/z', 'straddler', 99)",
        [],
    )?;
    Ok(())
}

/// HOLD-1 positive witness: a bounded import over a source whose declared
/// collation differs from the batch order preserves every row, field for
/// field, and survives a restart.
#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn a_bounded_import_preserves_every_row_when_source_collation_is_nocase() -> TestResult {
    let (root, db) = scratch("nocase")?;
    legacy_source(&db, "COLLATE NOCASE")?;
    let mut conn = Connection::open(&db)?;
    insert_boundary_rows(&mut conn)?;
    let source_rows: i64 = conn.query_row("SELECT COUNT(*) FROM session", [], |row| row.get(0))?;
    assert_eq!(
        source_rows, 513,
        "the fixture must straddle the batch bound"
    );
    drop(conn);

    // Deliberately NOT `?`. Under the collation mutant the import postcondition
    // fires and this call returns Err, and an Err returned through `?` leaves
    // the mutation harness no diagnostic to record — the row would be scored RED
    // with empty evidence, indistinguishable from any other row that fails the
    // same way. Naming the failure here is what keeps this row's attribution its
    // own.
    let outcome = match EventStore::migrate(&db, &root, "closure") {
        Ok(outcome) => outcome,
        Err(error) => panic!(
            "the bounded import must apply over a NOCASE-declared legacy source, got {error}"
        ),
    };
    assert!(
        matches!(outcome, MigrationOutcome::Applied { .. }),
        "the migration must apply"
    );

    let conn = Connection::open(&db)?;
    let projected: i64 =
        conn.query_row("SELECT COUNT(*) FROM session_list_projection", [], |row| {
            row.get(0)
        })?;
    assert_eq!(
        projected, source_rows,
        "the bounded import dropped a valid source row"
    );
    let baseline: i64 = conn.query_row(
        "SELECT COUNT(*) FROM session_list_legacy_baseline",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(baseline, source_rows, "every row needs its baseline");
    let recorded_rows: i64 = conn.query_row(
        "SELECT source_rows FROM session_list_import_meta",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        recorded_rows, projected,
        "recorded source domain must equal the projected domain"
    );

    // Exact field preservation for the straddling row: it is the one a
    // collation disagreement loses, so it carries distinct values in every
    // column rather than repeating the bulk run.
    let straddler: (
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        i64,
    ) = conn.query_row(
        "SELECT session_id, project_id, workspace_id, parent_id, directory, title, updated_ms
             FROM session_list_projection WHERE session_id = 'ses_a000'",
        [],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        },
    )?;
    assert_eq!(
        straddler,
        (
            "ses_a000".to_owned(),
            "prj_a".to_owned(),
            Some("ws_z".to_owned()),
            None,
            "/work/z".to_owned(),
            "straddler".to_owned(),
            99
        ),
        "the straddling row must survive with every field intact"
    );

    // No source row lost its content anywhere in the domain.
    let mismatched: i64 = conn.query_row(
        "SELECT COUNT(*) FROM session AS source
         LEFT JOIN session_list_projection AS projected
           ON projected.session_id = source.id COLLATE BINARY
         WHERE projected.session_id IS NULL
            OR projected.project_id <> source.project_id
            OR COALESCE(projected.workspace_id, '') <> COALESCE(source.workspace_id, '')
            OR COALESCE(projected.parent_id, '') <> COALESCE(source.parent_id, '')
            OR projected.directory <> source.directory
            OR projected.title <> source.title
            OR projected.updated_ms <> source.time_updated",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(mismatched, 0, "a source row changed or vanished on import");
    drop(conn);

    // Restart verification: the projection reopens without repair.
    let store = EventStore::open(&db, "closure")?;
    drop(store);
    let conn = Connection::open(&db)?;
    let after_restart: i64 =
        conn.query_row("SELECT COUNT(*) FROM session_list_projection", [], |row| {
            row.get(0)
        })?;
    assert_eq!(after_restart, source_rows, "restart lost projected rows");
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// The same law under the ordinary BINARY-declared source, so the NOCASE
/// witness above cannot pass merely because the import is order-insensitive.
#[test]
fn a_bounded_import_preserves_every_row_across_the_batch_bound_under_binary() -> TestResult {
    let (root, db) = scratch("binary")?;
    legacy_source(&db, "COLLATE BINARY")?;
    let mut conn = Connection::open(&db)?;
    insert_boundary_rows(&mut conn)?;
    drop(conn);

    let outcome = EventStore::migrate(&db, &root, "closure")?;
    assert!(matches!(outcome, MigrationOutcome::Applied { .. }));
    let conn = Connection::open(&db)?;
    let projected: i64 =
        conn.query_row("SELECT COUNT(*) FROM session_list_projection", [], |row| {
            row.get(0)
        })?;
    assert_eq!(projected, 513, "binary source lost a row across the bound");
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// HOLD-2 counterexample: a normal production open must not create an empty
/// projection with `empty-rust-store` provenance beside populated legacy rows.
/// An absent projection is not an empty page.
#[test]
fn a_production_open_refuses_to_manufacture_an_empty_projection_beside_legacy_rows() -> TestResult {
    let (root, db) = scratch("bypass")?;
    let store = EventStore::open(&db, "closure")?;
    drop(store);

    let conn = Connection::open(&db)?;
    conn.execute_batch(
        "DROP TRIGGER session_list_baseline_immutable_update;
         DROP TRIGGER session_list_baseline_immutable_delete;
         DROP TRIGGER session_list_meta_immutable_update;
         DROP TRIGGER session_list_meta_immutable_delete;
         DROP TABLE session_list_projection;
         DROP TABLE session_list_legacy_baseline;
         DROP TABLE session_list_import_meta;
         CREATE TABLE session (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            workspace_id TEXT,
            parent_id TEXT,
            directory TEXT NOT NULL,
            title TEXT NOT NULL,
            time_updated INTEGER NOT NULL
         );
         INSERT INTO session VALUES (
            'ses_legacy', 'prj_a', NULL, NULL, '/work/a', 'legacy', 42
         );",
    )?;
    drop(conn);

    let Err(error) = EventStore::open(&db, "closure") else {
        panic!("open manufactured an empty projection beside legacy source rows");
    };
    let refusal = error.to_string();
    // Assert the REASON, not merely that something failed: an open that
    // refused for an unrelated schema reason would satisfy `is_err` while
    // leaving this defect open.
    assert!(
        refusal.contains("legacy session source") && refusal.contains("ProductionMigrations"),
        "the refusal must name the legacy source and the admitted route: {refusal}"
    );

    // The refusal must leave legacy state neither hidden nor relabelled.
    let conn = Connection::open(&db)?;
    let legacy_rows: i64 = conn.query_row("SELECT COUNT(*) FROM session", [], |row| row.get(0))?;
    assert_eq!(legacy_rows, 1, "the refusal must not touch legacy rows");
    let manufactured: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name = 'session_list_import_meta'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        manufactured, 0,
        "the refusal must not leave an empty-rust-store provenance row behind"
    );
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// The refusal must be scoped to a PRESENT legacy source. A genuinely fresh
/// database has no legacy `session` table and must still open green, so the
/// guard cannot be satisfied by refusing every absent projection.
#[test]
fn a_fresh_database_without_legacy_state_still_opens_and_records_empty_provenance() -> TestResult {
    let (root, db) = scratch("fresh")?;
    let store = EventStore::open(&db, "closure")?;
    drop(store);
    let conn = Connection::open(&db)?;
    let domain: String = conn.query_row(
        "SELECT source_domain FROM session_list_import_meta",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        domain, "empty-rust-store",
        "a genuinely empty Rust store keeps its own provenance"
    );
    drop(conn);
    // And it reopens green.
    let reopened = EventStore::open(&db, "closure")?;
    drop(reopened);
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// The fact-log shape is verified BEFORE the session-list projection guard, and
/// this is the only input that can observe it.
///
/// An oracle-era database presents both problems at once: a pre-fencing
/// `event_sequence`/`event` pair AND a populated seven-column legacy `session`
/// table. Both refusals are therefore available, and which one answers is the
/// whole law — if the session-list guard ran first it would refuse, and every
/// fact-log column check would be recorded as enforced while never being
/// reached, for exactly the databases this store exists to migrate.
///
/// This asserts the MESSAGE, not `is_err()`. Both orders refuse, so a bare
/// `is_err` is green either way and proves nothing; only the identity of the
/// refusal separates them.
///
/// The two pre-existing open-refusal witnesses cannot stand in for this one.
/// Both build `CREATE TABLE session (id TEXT PRIMARY KEY)` — one column, zero
/// rows — which the narrow H2 guard admits at BOTH of its boundaries, so they
/// pass whichever order the call sits in.
#[test]
fn the_fact_log_column_check_refuses_before_the_session_list_guard_on_a_paired_source() -> TestResult
{
    let (root, db) = scratch("paired")?;
    // `legacy_source` builds the pre-fencing fact log: `event_sequence` has no
    // `aggregate_family` column, which is exactly what the column check names.
    legacy_source(&db, "COLLATE BINARY")?;
    let conn = Connection::open(&db)?;
    // Populated, so the session-list guard is genuinely ARMED and would refuse
    // if it were consulted first. An empty legacy table would disarm it and the
    // witness would pass for the wrong reason.
    conn.execute(
        "INSERT INTO session
            (id, project_id, workspace_id, parent_id, directory, title, time_updated)
         VALUES ('ses_paired', 'prj_a', NULL, NULL, '/work/a', 'legacy', 7)",
        [],
    )?;
    let armed: i64 = conn.query_row("SELECT COUNT(*) FROM session", [], |row| row.get(0))?;
    assert_eq!(
        armed, 1,
        "the session-list guard must be armed for this witness"
    );
    drop(conn);

    let Err(error) = EventStore::open(&db, "closure") else {
        panic!("a pre-fencing fact log beside a populated legacy source must refuse to open");
    };
    let refusal = error.to_string();
    assert!(
        refusal.contains("missing column aggregate_family"),
        "the fact-log COLUMN check must be what refuses, got {refusal}"
    );
    assert!(
        !refusal.contains("session-list projection is absent"),
        "the session-list guard must not answer before the fact log proves its own shape: {refusal}"
    );
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// Boundary 1: a table merely NAMED `session` is not a legacy source. It lacks
/// the legacy columns, so the migration would refuse it for missing columns —
/// refusing to OPEN over it would strand a database that no admitted path could
/// then repair. It opens, even holding rows.
#[test]
fn a_session_named_table_without_the_legacy_shape_is_not_a_legacy_source() -> TestResult {
    let (root, db) = scratch("stub")?;
    let conn = Connection::open(&db)?;
    conn.execute_batch(
        "CREATE TABLE session (id TEXT PRIMARY KEY);
         INSERT INTO session VALUES ('ses_stub');",
    )?;
    drop(conn);
    let store = EventStore::open(&db, "closure")?;
    drop(store);
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// Boundary 2: a legacy-SHAPED but EMPTY source has no rows to hide, so the
/// open is admitted. This is the stated edge of the guard — it refuses on
/// populated legacy state, which is the condition under which an
/// `empty-rust-store` projection would conceal shipped sessions.
#[test]
fn an_empty_legacy_shaped_source_has_nothing_to_hide_and_opens() -> TestResult {
    let (root, db) = scratch("emptylegacy")?;
    // Only the legacy `session` table: adding the oracle-era event tables
    // would make the fact-log column check refuse first and this witness would
    // never reach the session-list guard it exists to pin.
    let conn = Connection::open(&db)?;
    conn.execute_batch(
        "CREATE TABLE session (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            workspace_id TEXT,
            parent_id TEXT,
            directory TEXT NOT NULL,
            title TEXT NOT NULL,
            time_updated INTEGER NOT NULL
         );",
    )?;
    drop(conn);
    let store = EventStore::open(&db, "closure")?;
    drop(store);
    let conn = Connection::open(&db)?;
    let projected: i64 =
        conn.query_row("SELECT COUNT(*) FROM session_list_projection", [], |row| {
            row.get(0)
        })?;
    assert_eq!(projected, 0, "an empty source projects nothing");
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// After the admitted migration path has run, the legacy `session` table is
/// still present — so the guard must key on the missing projection, not on the
/// mere existence of legacy state, or every migrated database would refuse to
/// reopen.
#[test]
fn a_migrated_database_reopens_green_even_though_legacy_rows_remain() -> TestResult {
    let (root, db) = scratch("migrated")?;
    legacy_source(&db, "COLLATE BINARY")?;
    let conn = Connection::open(&db)?;
    conn.execute(
        "INSERT INTO session
            (id, project_id, workspace_id, parent_id, directory, title, time_updated)
         VALUES ('ses_a', 'prj_a', NULL, NULL, '/work/a', 'A', 7)",
        [],
    )?;
    drop(conn);
    let outcome = EventStore::migrate(&db, &root, "closure")?;
    assert!(matches!(outcome, MigrationOutcome::Applied { .. }));

    let store = EventStore::open(&db, "closure")?;
    drop(store);
    let conn = Connection::open(&db)?;
    let projected: i64 =
        conn.query_row("SELECT COUNT(*) FROM session_list_projection", [], |row| {
            row.get(0)
        })?;
    assert_eq!(projected, 1, "the migrated projection must survive reopen");
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn short_query_budget_interrupts_releases_and_rearms() -> TestResult {
    let (root, db) = scratch("short-budget")?;
    let mut store = EventStore::open(&db, "budget-test")?;
    for (steps, finish) in [(1, false), (999, true), (1, true)] {
        let budget = SqliteVmBudget::new(steps).ok_or("positive budget rejected")?;
        let guard = store.read_budget(budget)?;
        let result = guard
            .connection()
            .query_row("SELECT 1", [], |row| row.get::<_, i64>(0));
        if steps == 1 {
            assert!(
                matches!(result, Err(rusqlite::Error::SqliteFailure(ref error, _))
                    if error.code == rusqlite::ErrorCode::OperationInterrupted),
                "one-step short query must interrupt: {result:?}"
            );
            assert!(guard.exhausted(), "interruption lost exhaustion state");
        } else {
            assert_eq!(result?, 1);
            assert!(!guard.exhausted(), "generous short query exhausted");
        }
        if finish {
            guard.finish()?;
        } else {
            drop(guard);
        }
        let sibling: i64 = store
            .connection()
            .query_row("SELECT 1", [], |row| row.get(0))?;
        assert_eq!(sibling, 1, "released budget interrupted sibling query");
    }
    drop(store);
    std::fs::remove_dir_all(root)?;
    Ok(())
}
