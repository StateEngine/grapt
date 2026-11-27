//! Ordinary startup must exercise both platform inspection and the owned opener.
use super::shared::{scratch, FixedAuthorities, TestResult};
use grapt_platform::{FirstContactKind, PlatformFirstContact, StoreDirective};
use grapt_store::{preflight_read_only, OwnedEventStore, StoreReadiness};
use grapt_types::ClockAuthority;
use rusqlite::Connection;
use std::path::Path;

fn open_ready(root: &Path) -> TestResult<OwnedEventStore> {
    let inspected = PlatformFirstContact::inspect(root);
    assert_eq!(inspected.kind(), FirstContactKind::Ready);
    Ok(inspected.open_with_authorities(
        StoreDirective::OpenReady,
        "warm-ready-integration",
        &FixedAuthorities,
        &FixedAuthorities,
    )?)
}

#[test]
fn a_large_ready_store_opens_without_a_historical_integrity_walk() -> TestResult {
    let root = scratch("large-ready-history")?;
    let database = root.join("database.sqlite3");
    drop(PlatformFirstContact::inspect(&root).open_with_authorities(
        StoreDirective::Initialize,
        "warm-ready-integration",
        &FixedAuthorities,
        &FixedAuthorities,
    )?);
    let conn = Connection::open(&database)?;
    conn.execute(
        "INSERT INTO event_sequence (aggregate_family,aggregate_id,seq,owner_id)
         VALUES ('session','ses_history',1000000,NULL)",
        [],
    )?;
    conn.execute(
        "INSERT INTO event (id,aggregate_family,aggregate_id,seq,created,type,data,writer_epoch)
         SELECT printf('evt_history_%07d', n), 'session', 'ses_history', n, 0,
                'history.synthetic.1', '{}', 1
         FROM (WITH RECURSIVE gen(n) AS (
           SELECT 1 UNION ALL SELECT n+1 FROM gen WHERE n<1000000) SELECT n FROM gen)",
        [],
    )?;
    drop(conn);
    // An explicit full inspection of a million-event store is either a finished
    // walk or a spent budget, and never corruption: the store is healthy, and
    // only the check's own bound can be the reason it did not finish. Calling
    // that Integrity told operators a usable store was unreadable.
    let inspected = preflight_read_only(&database);
    assert!(
        matches!(inspected, StoreReadiness::Ready | StoreReadiness::Slow),
        "a million-event store must never read as corrupt: {inspected:?}"
    );
    let clock = grapt_store::SystemClock;
    let started = clock.now().as_millis();
    let opened = open_ready(&root)?;
    let elapsed = clock.now().as_millis() - started;
    assert!(
        (0..=5000).contains(&elapsed),
        "ready open took {elapsed} ms"
    );
    eprintln!("million-event ready open: {elapsed} ms");
    drop(opened);

    // Case-insensitive collision protection still refuses before probing and
    // leaves the foreign row intact. This is not a weaker prefix comparison.
    let conn = Connection::open(&database)?;
    conn.execute(
        "UPDATE event SET id='GRAPT-STORE SCHEMA PROBE foreign' WHERE id='evt_history_0000001'",
        [],
    )?;
    drop(conn);
    assert!(open_ready(&root).is_err());
    let conn = Connection::open(&database)?;
    let preserved: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM event WHERE id='GRAPT-STORE SCHEMA PROBE foreign')",
        [],
        |r| r.get(0),
    )?;
    assert!(preserved);
    conn.execute(
        "UPDATE event SET id='evt_history_0000001' WHERE id='GRAPT-STORE SCHEMA PROBE foreign'",
        [],
    )?;
    // A foreign index is not replaced or silently trusted on a ready open.
    conn.execute("DROP INDEX event_probe_id_idx", [])?;
    conn.execute(
        "CREATE INDEX event_probe_id_idx ON event(id COLLATE BINARY)",
        [],
    )?;
    drop(conn);
    assert!(open_ready(&root).is_err());
    let conn = Connection::open(&database)?;
    let collation: String = conn.query_row(
        "SELECT coll FROM pragma_index_xinfo('event_probe_id_idx') WHERE key=1",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(collation, "BINARY");
    drop(conn);
    std::fs::remove_dir_all(root)?;
    Ok(())
}
