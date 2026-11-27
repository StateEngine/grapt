//! Independent read leases preserve snapshots without taking writer ownership.
use grapt_store::read_pool::{ReadPool, ReadPoolError, READ_CAPACITY, READ_WAIT};
use grapt_store::{AggregateId, EventStore, OsRandomIds};
use grapt_types::IdAuthority;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Root(PathBuf);

impl Root {
    fn new() -> TestResult<Self> {
        let ids = OsRandomIds::seed()?;
        let root = std::env::temp_dir().join(format!("read-pool-{}", ids.mint().as_str()));
        std::fs::create_dir(&root)?;
        Ok(Self(root))
    }

    fn open(&self) -> TestResult<(EventStore, ReadPool)> {
        let store = EventStore::open(&self.0.join("events.db"), "read-pool-fixture")?;
        let pool = store.read_pool()?.ok_or("file store has no reader pool")?;
        Ok((store, pool))
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

fn count(store: &EventStore) -> rusqlite::Result<i64> {
    store
        .connection()
        .query_row("SELECT COUNT(*) FROM aggregate_writer", [], |row| {
            row.get(0)
        })
}

fn elect(store: &mut EventStore, suffix: &str) -> TestResult {
    store.elect_writer(&AggregateId::Session(
        format!("ses_reader_{suffix}").parse()?,
    ))?;
    Ok(())
}

#[test]
fn a_reader_finishes_while_the_writer_mutex_and_immediate_transaction_are_held() -> TestResult {
    let root = Root::new()?;
    let (store, pool) = root.open()?;
    let writer = Arc::new(Mutex::new(store));
    let guard = writer.lock().map_err(|_| "writer mutex poisoned")?;
    let before = count(&guard)?;
    guard.connection().execute_batch("BEGIN IMMEDIATE")?;
    let (done, received) = mpsc::sync_channel(1);
    let task = std::thread::spawn(move || {
        let result = (|| -> Result<i64, String> {
            let read = pool.checkout().map_err(|error| error.to_string())?;
            let value = count(read.store()).map_err(|error| error.to_string())?;
            read.finish().map_err(|error| error.to_string())?;
            Ok(value)
        })();
        let _sent = done.send(result);
    });
    let result = received.recv_timeout(Duration::from_secs(3));
    guard.connection().execute_batch("ROLLBACK")?;
    drop(guard);
    task.join().map_err(|_| "reader panicked")?;
    assert_eq!(
        result??, before,
        "reader did not finish independently of writer ownership"
    );
    Ok(())
}

#[test]
fn each_checkout_pins_one_snapshot_and_recycled_connections_see_new_commits() -> TestResult {
    let root = Root::new()?;
    let (mut writer, pool) = root.open()?;
    let before = count(&writer)?;
    let read = pool.checkout()?;
    // A checkout pins before the consumer's first query, not on that query.
    elect(&mut writer, "first")?;
    assert_eq!(count(read.store())?, before);
    elect(&mut writer, "second")?;
    assert_eq!(count(read.store())?, before);
    read.finish()?;
    let next = pool.checkout()?;
    assert_eq!(
        count(next.store())?,
        before + 2,
        "an idle connection retained its prior snapshot"
    );
    next.finish()?;
    Ok(())
}

#[test]
fn saturated_clones_wait_with_a_deadline_while_the_writer_commits() -> TestResult {
    let root = Root::new()?;
    let (mut writer, pool) = root.open()?;
    let mut held = Vec::new();
    for _ in 0..READ_CAPACITY {
        held.push(pool.checkout()?);
    }
    let another = writer.read_pool()?.ok_or("shared reader pool absent")?;
    let (started, ready) = mpsc::sync_channel(1);
    let (done, received) = mpsc::sync_channel(1);
    let task = std::thread::spawn(move || {
        let _sent = started.send(());
        let _sent = done.send(matches!(another.checkout(), Err(ReadPoolError::Busy)));
    });
    ready.recv_timeout(Duration::from_secs(3))?;
    assert!(
        matches!(
            received.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "a saturated pool refused without waiting or admitted another reader"
    );
    elect(&mut writer, "during_wait")?;
    let busy = received.recv_timeout(READ_WAIT + Duration::from_secs(3));
    drop(held);
    task.join().map_err(|_| "waiting reader panicked")?;
    assert!(busy?, "saturated pool did not return its capacity refusal");
    pool.checkout()?.finish()?;
    Ok(())
}

#[test]
fn released_capacity_wakes_a_waiting_reader_before_its_deadline() -> TestResult {
    let root = Root::new()?;
    let (_writer, pool) = root.open()?;
    let mut held = Vec::new();
    for _ in 0..READ_CAPACITY {
        held.push(pool.checkout()?);
    }
    let waiting = pool.clone();
    let (done, received) = mpsc::sync_channel(1);
    let task = std::thread::spawn(move || {
        let result = waiting
            .checkout()
            .and_then(|read| read.finish())
            .map_err(|error| error.to_string());
        let _sent = done.send(result);
    });
    assert!(matches!(
        received.recv_timeout(Duration::from_millis(30)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    held.pop().ok_or("held reader missing")?.finish()?;
    let result = received.recv_timeout(Duration::from_secs(3));
    drop(held);
    task.join().map_err(|_| "waiting reader panicked")?;
    result??;
    Ok(())
}

#[test]
fn reader_unwind_and_rollback_failure_do_not_leak_capacity() -> TestResult {
    let root = Root::new()?;
    let (_writer, pool) = root.open()?;
    for _ in 0..READ_CAPACITY * 2 {
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _read = pool
                .checkout()
                .unwrap_or_else(|error| panic!("checkout: {error}"));
            panic!("injected reader unwind");
        }));
        let payload = unwind.err().ok_or("reader did not unwind")?;
        assert_eq!(
            payload.downcast_ref::<&str>(),
            Some(&"injected reader unwind")
        );
        let read = pool.checkout()?;
        // Ending the transaction simulates a projection cleanup defect.
        read.store().connection().execute_batch("ROLLBACK")?;
        assert!(read.finish().is_err(), "cleanup failure was hidden");
    }
    // Discarding returns its capacity exactly once, like every other ending:
    // a leak here would strand a reader for the life of the pool.
    for _ in 0..READ_CAPACITY * 2 {
        pool.checkout()?.discard();
    }
    let mut held = Vec::new();
    for _ in 0..READ_CAPACITY {
        held.push(pool.checkout()?);
    }
    Ok(())
}

#[test]
fn leased_connections_refuse_writes_and_do_not_create_missing_databases() -> TestResult {
    let root = Root::new()?;
    let (writer, pool) = root.open()?;
    let read = pool.checkout()?;
    assert!(read.store().connection().is_readonly(rusqlite::MAIN_DB)?);
    let query_only: bool =
        read.store()
            .connection()
            .pragma_query_value(None, "query_only", |row| row.get(0))?;
    assert!(query_only);
    assert!(read
        .store()
        .connection()
        .execute_batch("CREATE TABLE escaped(value)")
        .is_err());
    read.finish()?;
    drop(writer);
    #[cfg(unix)]
    {
        std::fs::remove_file(root.0.join("events.db"))?;
        assert!(
            pool.checkout().is_err(),
            "missing database was recreated or stale data was served"
        );
        assert!(!root.0.join("events.db").exists());
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_replaced_database_is_refused_even_with_a_cached_reader() -> TestResult {
    let root = Root::new()?;
    let (writer, pool) = root.open()?;
    pool.checkout()?.finish()?;
    drop(writer);
    let replacement = root.0.join("replacement.db");
    drop(EventStore::open(&replacement, "replacement-fixture")?);
    std::fs::rename(root.0.join("events.db"), root.0.join("original.db"))?;
    std::fs::rename(replacement, root.0.join("events.db"))?;
    assert!(
        matches!(pool.checkout(), Err(ReadPoolError::Replaced)),
        "cached reader accepted a different file at the database path"
    );
    Ok(())
}

#[test]
fn a_discarded_reader_returns_its_capacity_without_handing_on_its_connection() -> TestResult {
    let root = Root::new()?;
    let (_writer, pool) = root.open()?;
    // Mark this connection so a later checkout can prove whose it holds.
    let failed = pool.checkout()?;
    failed
        .store()
        .connection()
        .pragma_update(None, "cache_size", -4321)?;
    // A reader that met an error ends without recycling its connection.
    failed.discard();
    // The capacity comes straight back...
    let mut held = Vec::new();
    for _ in 0..READ_CAPACITY {
        held.push(pool.checkout()?);
    }
    // ...and no checkout inherited the discarded connection.
    for read in &held {
        let marker: i64 =
            read.store()
                .connection()
                .pragma_query_value(None, "cache_size", |row| row.get(0))?;
        assert_ne!(
            marker, -4321,
            "a discarded connection was handed to a later checkout"
        );
    }
    Ok(())
}

#[test]
fn checkout_budget_bounds_sqlite_wait_without_shortening_pinned_queries() -> TestResult {
    let root = Root::new()?;
    let (writer, pool) = root.open()?;
    drop(writer);
    let locker = rusqlite::Connection::open(root.0.join("events.db"))?;
    locker.execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE")?;
    let waiting = pool.clone();
    let (done, received) = mpsc::sync_channel(1);
    let task = std::thread::spawn(move || {
        let result = waiting.checkout_within(|| Duration::from_millis(25));
        let _sent = done.send(matches!(result, Err(ReadPoolError::Busy)));
    });
    let busy = received.recv_timeout(Duration::from_millis(750));
    locker.execute_batch("ROLLBACK")?;
    drop(locker);
    task.join().map_err(|_| "waiting reader panicked")?;
    assert!(busy?, "SQLite contention did not return a capacity refusal");
    let read = pool.checkout_within(|| Duration::ZERO)?;
    let timeout: u32 =
        read.store()
            .connection()
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))?;
    assert_eq!(u128::from(timeout), READ_WAIT.as_millis());
    read.finish()?;
    Ok(())
}
