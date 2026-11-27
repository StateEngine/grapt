//! SC-REL-04: connection policy, concurrent readers, and process-crash recovery.
//! NORMAL preserves SQLite consistency, but a power failure may lose recent
//! ordinary commits since the last checkpoint. Intent transactions select FULL
//! before BEGIN, so COMMIT syncs the WAL without a fallible checkpoint afterward.
use grapt_store::{AggregateId, EventStore};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

type TestResult = Result<(), Box<dyn std::error::Error>>;
fn root(label: &str) -> Result<PathBuf, std::io::Error> {
    let root = std::env::temp_dir().join(format!("grapt-wal-{label}-{}", std::process::id()));
    std::fs::create_dir_all(&root)?;
    Ok(root)
}
fn assert_policy(conn: &Connection, expected: &str) -> TestResult {
    let journal: String = conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    let synchronous: i64 = conn.pragma_query_value(None, "synchronous", |row| row.get(0))?;
    assert_eq!(journal, expected);
    assert_eq!(synchronous, 1);
    Ok(())
}
#[test]
fn file_connections_reopen_with_wal_normal_and_memory_stays_memory() -> TestResult {
    let root = root("policy")?;
    let path = root.join("events.db");
    for _ in 0..2 {
        let store = EventStore::open(&path, "wal-policy")?;
        assert_policy(store.connection(), "wal")?;
    }
    assert_policy(EventStore::open_in_memory()?.connection(), "memory")?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}
#[test]
fn a_live_read_snapshot_does_not_block_a_committed_writer() -> TestResult {
    let root = root("reader")?;
    let path = root.join("events.db");
    let mut store = EventStore::open(&path, "wal-reader")?;
    let reader = Connection::open(&path)?;
    reader.execute_batch("BEGIN")?;
    let count = |conn: &Connection| {
        conn.query_row("SELECT COUNT(*) FROM aggregate_writer", [], |row| {
            row.get::<_, i64>(0)
        })
    };
    let before = count(&reader)?;
    store.elect_writer(&AggregateId::Session("ses_wal_reader".parse()?))?;
    assert_eq!(count(&reader)?, before, "the reader retains its snapshot");
    reader.execute_batch("COMMIT")?;
    assert_eq!(
        count(&reader)?,
        before + 1,
        "the next read sees the committed writer"
    );
    drop(reader);
    drop(store);
    std::fs::remove_dir_all(root)?;
    Ok(())
}
#[test]
fn a_killed_wal_writer_preserves_commit_and_rolls_back_incomplete_write() -> TestResult {
    let root = root("crash")?;
    let path = root.join("events.db");
    let marker = root.join("uncommitted.marker");
    let mut child = Command::new(std::env::current_exe()?)
        .args(["--exact", "wal_crash_writer"])
        .env("SC_WAL_CRASH_DB", &path)
        .env("SC_WAL_CRASH_MARKER", &marker)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut observed = false;
    for _ in 0..1000 {
        if marker.exists() {
            observed = true;
            break;
        }
        if let Some(status) = child.try_wait()? {
            panic!("WAL writer exited before tear: {status}");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let _ = child.kill();
    child.wait()?;
    assert!(
        observed,
        "WAL writer did not reach the incomplete transaction"
    );
    assert!(
        Path::new(&format!("{}-wal", path.display())).is_file(),
        "the crash must leave a WAL to recover"
    );
    let store = EventStore::open(&path, "wal-recovery")?;
    assert_policy(store.connection(), "wal")?;
    let epoch: i64 = store.connection().query_row(
        "SELECT epoch FROM aggregate_writer WHERE aggregate_id = 'ses_wal_crash'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        epoch, 1,
        "the committed election survives; the uncommitted increment does not"
    );
    let integrity: String = store
        .connection()
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    assert_eq!(integrity, "ok");
    drop(store);
    std::fs::remove_dir_all(root)?;
    Ok(())
}
#[test]
fn wal_crash_writer() -> TestResult {
    let Some(path) = std::env::var_os("SC_WAL_CRASH_DB") else {
        return Ok(());
    };
    let marker = std::env::var_os("SC_WAL_CRASH_MARKER").ok_or("missing WAL crash marker")?;
    let mut store = EventStore::open(Path::new(&path), "wal-crash")?;
    assert_policy(store.connection(), "wal")?;
    store.elect_writer(&AggregateId::Session("ses_wal_crash".parse()?))?;
    // A separate, unguarded fixture connection represents a writer killed
    // mid-transaction; production commits above still used the governed owner.
    let mut foreign = Connection::open(Path::new(&path))?;
    let tx = foreign.transaction()?;
    tx.execute(
        "UPDATE aggregate_writer SET epoch = epoch + 1 WHERE aggregate_id = 'ses_wal_crash'",
        [],
    )?;
    let epoch: i64 = tx.query_row(
        "SELECT epoch FROM aggregate_writer WHERE aggregate_id = 'ses_wal_crash'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(epoch, 2);
    std::fs::write(marker, "uncommitted\n")?;
    std::thread::sleep(std::time::Duration::from_secs(120));
    panic!("WAL writer was never killed");
}

#[test]
fn transaction_policy_is_chosen_before_begin_and_streams_cannot_upgrade() -> TestResult {
    let root = root("barrier")?;
    let path = root.join("events.db");
    let mut store = EventStore::open(&path, "wal-barrier")?;
    let tx = store.stream_transaction()?;
    let synchronous: i64 = tx.pragma_query_value(None, "synchronous", |row| row.get(0))?;
    assert_eq!(synchronous, 1);
    assert!(tx.require_durable_commit().is_err());
    tx.commit()?.discard();
    let tx = store.governed_transaction()?;
    let synchronous: i64 = tx.pragma_query_value(None, "synchronous", |row| row.get(0))?;
    assert_eq!(
        synchronous, 2,
        "intent WAL commit syncs the log before acknowledgment"
    );
    tx.require_durable_commit()?;
    tx.commit()?.discard();
    drop(store);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_reader_blocking_checkpoint_cannot_turn_a_committed_intent_into_an_error() -> TestResult {
    let root = root("reader-intent")?;
    let path = root.join("events.db");
    let mut store = EventStore::open(&path, "wal-reader-intent")?;
    store.connection().execute_batch("PRAGMA wal_autocheckpoint=0; CREATE TABLE wal_sync_probe(value INTEGER); PRAGMA wal_checkpoint(FULL)")?;
    store.connection().busy_timeout(std::time::Duration::ZERO)?;
    let reader = Connection::open(&path)?;
    reader.execute_batch("BEGIN; SELECT * FROM wal_sync_probe")?;
    let tx = store.governed_transaction()?;
    tx.execute("INSERT INTO wal_sync_probe VALUES (1)", [])?;
    tx.commit()?.discard();
    let count = |conn: &Connection| {
        conn.query_row("SELECT COUNT(*) FROM wal_sync_probe", [], |row| {
            row.get::<_, i64>(0)
        })
    };
    assert_eq!(count(&reader)?, 0);
    assert_eq!(count(store.connection())?, 1);
    reader.execute_batch("COMMIT")?;
    assert_eq!(count(&reader)?, 1);
    drop(reader);
    drop(store);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_provider_send_permit_requires_a_full_intent_transaction() -> TestResult {
    use grapt_store::attempt_receipt::{AttemptRecordCeiling, ReserveInput, TerminalCoordinates};
    use grapt_types::{
        BoundedAttemptId, BoundedSessionId, DurableAttemptReceipt, RequestSentMarker,
        ReservedMarker, Timestamp,
    };
    let root = root("permit")?;
    let path = root.join("events.db");
    let mut store = EventStore::open(&path, "wal-permit")?;
    store.connection().execute_batch("PRAGMA wal_autocheckpoint=0; CREATE TABLE wal_sync_probe(value INTEGER); PRAGMA wal_checkpoint(FULL)")?;
    let session: grapt_types::SessionId = "ses_wal_permit".parse()?;
    let aggregate = AggregateId::Session(session.clone());
    store.elect_writer(&aggregate)?;
    let writer = store.fencing_token(&aggregate)?;
    let attempt = grapt_types::PhysicalAttemptId::new("wal-permit-attempt").ok_or("attempt")?;
    let maximum = std::num::NonZeroU32::new(4096).ok_or("bound")?;
    let ceiling = AttemptRecordCeiling::new(maximum);
    let receipt = DurableAttemptReceipt::reserved(
        BoundedAttemptId::new(attempt.clone(), maximum).map_err(|_| "attempt bound")?,
        BoundedSessionId::new(session, maximum).map_err(|_| "session bound")?,
        grapt_types::StepNumber::new(1).ok_or("step")?,
        grapt_types::AttemptOrdinal::new(1).ok_or("ordinal")?,
        ReservedMarker::new(Timestamp::from_millis(1)),
    );
    let terminal = TerminalCoordinates::new("evt_wal_permit_terminal".parse()?, 3);
    let tx = store.stream_transaction()?;
    assert!(
        EventStore::reserve_attempt_receipt_within(
            &tx,
            &ReserveInput {
                receipt: &receipt,
                writer,
                terminal: &terminal,
                ceiling,
            }
        )
        .is_err(),
        "a stream cannot mutate operational authority rows"
    );
    tx.commit()?.discard();
    store.reserve_attempt_receipt(&ReserveInput {
        receipt: &receipt,
        writer: store.fencing_token(&aggregate)?,
        terminal: &terminal,
        ceiling,
    })?;
    let tx = store.stream_transaction()?;
    assert!(
        EventStore::permit_attempt_send_within(
            &tx,
            &attempt,
            writer,
            ceiling,
            RequestSentMarker::new(Timestamp::from_millis(2)),
        )
        .is_err(),
        "a stream transaction cannot grant permission to emit external bytes"
    );
    tx.commit()?.discard();
    let tx = store.governed_transaction()?;
    let synchronous: i64 = tx.pragma_query_value(None, "synchronous", |row| row.get(0))?;
    assert_eq!(synchronous, 2);
    EventStore::permit_attempt_send_within(
        &tx,
        &attempt,
        writer,
        ceiling,
        RequestSentMarker::new(Timestamp::from_millis(2)),
    )?;
    tx.commit()?.discard();
    let sent: i64 = store.connection().query_row(
        "SELECT request_sent_ms FROM attempt_receipt WHERE attempt_id='wal-permit-attempt'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(sent, 2);
    drop(store);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_stream_refuses_domain_events_before_allocating_rows_even_if_caller_commits() -> TestResult {
    let root = root("stream-refusal")?;
    let path = root.join("events.db");
    let mut store = EventStore::open(&path, "stream-refusal")?;
    let session: grapt_types::SessionId = "ses_stream_refusal".parse()?;
    let aggregate = AggregateId::Session(session.clone());
    store.elect_writer(&aggregate)?;
    let writer = store.fencing_token(&aggregate)?;
    let tx = store.stream_transaction()?;
    let refusal = EventStore::append_within(
        &tx,
        grapt_store::AppendInput {
            id: &"evt_stream_refusal".parse()?,
            aggregate,
            writer,
            kind_name: "session.renamed",
            version: grapt_types::EventVersion::new(1).ok_or("version")?,
            created_ms: 1,
            data: &serde_json::json!({"sessionID":session.as_str(),"title":"refused"}),
        },
        &mut [],
    );
    assert!(refusal.is_err());
    tx.commit()?.discard();
    let rows: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE aggregate_id=?1",
        [session.as_str()],
        |row| row.get(0),
    )?;
    let sequences: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event_sequence WHERE aggregate_id=?1",
        [session.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!((rows, sequences), (0, 0));
    drop(store);
    std::fs::remove_dir_all(root)?;
    Ok(())
}
