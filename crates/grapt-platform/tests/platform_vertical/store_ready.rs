//! Ordinary startup must exercise both platform inspection and the owned opener.
use super::shared::{legacy_database, scratch, FixedAuthorities, TestResult};
use grapt_platform::{FirstContactKind, PlatformFirstContact, PlatformStoreRefusal};
use grapt_store::{preflight_read_only, OwnedEventStore, StoreReadiness};
use grapt_types::ClockAuthority;
use rusqlite::Connection;
use std::path::Path;

fn open_ready(root: &Path) -> TestResult<OwnedEventStore> {
    let inspected = PlatformFirstContact::inspect(root);
    assert_eq!(inspected.kind(), FirstContactKind::Ready);
    Ok(inspected.open_with_authorities(
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

/// A store another process created after first contact is opened as it is now.
#[test]
fn a_store_created_after_first_contact_is_opened() -> TestResult {
    let root = scratch("concurrent-creator")?;
    let late = PlatformFirstContact::inspect(&root);
    assert_eq!(late.kind(), FirstContactKind::Absent);
    drop(open_ready_or_create(PlatformFirstContact::inspect(&root))?);
    let opened = open_ready_or_create(late);
    assert!(
        matches!(&opened, Ok(owner) if matches!(owner.receipt(), grapt_store::OwnedOpenReceipt::OpenedReady)),
        "a store created by another process was not opened: {opened:?}"
    );
    drop(opened);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

/// A store that disappears before its migration is refused, never replaced
/// by a new empty one.
#[test]
fn a_store_that_vanishes_before_migration_is_not_recreated() -> TestResult {
    let root = scratch("vanished-before-migration")?;
    let database = root.join("database.sqlite3");
    legacy_database(&database)?;
    let found = PlatformFirstContact::inspect(&root);
    assert_eq!(found.kind(), FirstContactKind::MigrationNeeded);
    std::fs::remove_file(&database)?;
    let opened = open_ready_or_create(found);
    assert!(
        matches!(opened, Err(PlatformStoreRefusal::Receded { .. })),
        "a vanished store was not refused: {opened:?}"
    );
    assert!(
        !database.exists(),
        "an empty store replaced the vanished one"
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}

fn open_ready_or_create(
    found: PlatformFirstContact,
) -> Result<OwnedEventStore, PlatformStoreRefusal> {
    found.open_with_authorities(
        "warm-ready-integration",
        &FixedAuthorities,
        &FixedAuthorities,
    )
}
