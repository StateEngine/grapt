use super::*;
use crate::{AggregateId, EventStore, ReserveInput, TerminalCoordinates};
use grapt_types::*;
use std::num::NonZeroU32;
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn sc_acct01_malformed_accounting_tables_refuse_before_index_creation() -> TestResult {
    for (table, missing, schema) in [
        ("provider_attempt_accounting", "session_id",
         "CREATE TABLE provider_attempt_accounting(attempt_id TEXT PRIMARY KEY, record_bytes BLOB)"),
        ("provider_attempt_coverage", "complete",
         "CREATE TABLE provider_attempt_coverage(session_id TEXT PRIMARY KEY, payload TEXT)"),
    ] {
        let path = std::env::temp_dir().join(format!("acct01-shape-{table}-{}.sqlite", std::process::id()));
        assert!(!path.exists(), "preserve the existing fixture");
        {
            let conn = rusqlite::Connection::open(&path)?;
            conn.execute_batch(schema)?;
        }
        let error = match EventStore::open(&path, "acct01-shape") {
            Ok(_) => panic!("malformed {table} opened"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains(&format!("incompatible preexisting table {table}")), "{error}");
        assert!(error.contains(&format!("missing column {missing}")), "{error}");
        std::fs::remove_file(path)?;
    }
    Ok(())
}

#[test]
fn sc_acct01_send_permit_and_accounting_survive_or_rollback_together() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let session: SessionId = "ses_acct01".parse()?;
    let attempt = PhysicalAttemptId::new("pat_acct01").ok_or("attempt")?;
    let bound = NonZeroU32::new(4096).ok_or("bound")?;
    let ceiling = AttemptRecordCeiling::new(bound);
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    let writer = store.fencing_token(&AggregateId::Session(session.clone()))?;
    let receipt = DurableAttemptReceipt::reserved(
        BoundedAttemptId::new(attempt.clone(), bound).map_err(|e| format!("{e:?}"))?,
        BoundedSessionId::new(session, bound).map_err(|e| format!("{e:?}"))?,
        StepNumber::new(1).ok_or("step")?,
        AttemptOrdinal::new(1).ok_or("ordinal")?,
        ReservedMarker::new(Timestamp::from_millis(10)),
    );
    let terminal = TerminalCoordinates::new("evt_acct01".parse()?, 30);
    store.reserve_attempt_receipt(&ReserveInput {
        receipt: &receipt,
        writer,
        terminal: &terminal,
        ceiling,
    })?;
    let permit = RequestSentMarker::new(Timestamp::from_millis(20));
    {
        let tx = store.governed_transaction()?;
        EventStore::permit_attempt_send_within(&tx, &attempt, writer, ceiling, permit.clone())?;
        // Death before commit must leave neither half visible on restart.
    }
    assert!(store
        .attempt_receipt(&attempt, ceiling)?
        .ok_or("receipt")?
        .receipt()
        .request_sent_at()
        .is_none());
    let count: i64 = store.connection().query_row(
        "SELECT count(*) FROM provider_attempt_accounting",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(count, 0);
    let tx = store.governed_transaction()?;
    EventStore::permit_attempt_send_within(&tx, &attempt, writer, ceiling, permit.clone())?;
    tx.commit()?.discard();
    let tx = store.governed_transaction()?;
    EventStore::permit_attempt_send_within(&tx, &attempt, writer, ceiling, permit)?;
    tx.commit()?.discard();
    let bytes: Vec<u8> = store.connection().query_row(
        "SELECT record_bytes FROM provider_attempt_accounting",
        [],
        |r| r.get(0),
    )?;
    let record = decode_record(&bytes, ceiling)?;
    assert_eq!(
        record.receipt().request_sent_at(),
        Some(Timestamp::from_millis(20))
    );
    let count: i64 = store.connection().query_row(
        "SELECT count(*) FROM provider_attempt_accounting",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(count, 1, "exact retry duplicated the dispatch");
    for sql in [
        "DELETE FROM provider_attempt_accounting",
        "UPDATE provider_attempt_coverage SET complete=0",
    ] {
        assert!(
            store.connection().execute(sql, []).is_err(),
            "consumer rewrote accounting truth: {sql}"
        );
    }
    Ok(())
}

#[test]
fn sc_acct01_upgrade_keeps_historical_gaps_and_installation_is_atomic() -> TestResult {
    let mut conn = rusqlite::Connection::open_in_memory()?;
    conn.execute_batch(
        "CREATE TABLE event(aggregate_family TEXT, aggregate_id TEXT);
        CREATE TABLE attempt_receipt(session_id TEXT);
        CREATE TABLE attempt_receipt_quarantine(session_id TEXT);
        INSERT INTO event VALUES('session','ses_old');
        INSERT INTO attempt_receipt VALUES('ses_receipt_only');",
    )?;
    {
        let tx = conn.transaction()?;
        ensure_schema(&tx)?;
    }
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='provider_attempt_coverage')",
        [],
        |r| r.get(0),
    )?;
    assert!(
        !exists,
        "interrupted installation committed a coverage claim"
    );
    let tx = conn.transaction()?;
    ensure_schema(&tx)?;
    tx.commit()?;
    conn.execute("INSERT INTO event VALUES('session','ses_new')", [])?;
    let tx = conn.transaction()?;
    ensure_schema(&tx)?;
    tx.commit()?;
    let rows: Vec<(String, i64)> = conn
        .prepare("SELECT session_id,complete FROM provider_attempt_coverage ORDER BY session_id")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    assert_eq!(
        rows,
        vec![("ses_old".into(), 0), ("ses_receipt_only".into(), 0)]
    );
    Ok(())
}
