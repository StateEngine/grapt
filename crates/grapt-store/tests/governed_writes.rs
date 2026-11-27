//! The write chokepoint, attacked from where a consumer actually stands.
//!
//! These are integration tests on purpose. Everything here reaches
//! `grapt-store` through its PUBLIC surface only, which is the same surface
//! `grapt-inbox`, `grapt-form`, `grapt-shell`, `grapt-subagent`,
//! `grapt-permission` and `grapt-instruction` have. A unit test inside the
//! crate can name `WriteGate` and lift the denial; a consumer cannot, and that
//! difference is the whole claim.
//!
//! Each test states the exact refusal it expects. `Authorization::Deny` becomes
//! `SQLITE_AUTH` at statement PREPARATION, so a denied statement never runs and
//! never touches a row — asserting only "the row did not change" would score an
//! `Authorization::Ignore` (a silent skip) as a pass.

use grapt_store::{
    AggregateId, AppendInput, DecodedEvent, EventStore, ProjectionId, Projector, WriteError,
};
use grapt_types::{EventId, EventVersion, SessionId};
use rusqlite::ErrorCode;
use serde_json::json;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn session(id: &str) -> Result<AggregateId, Box<dyn std::error::Error>> {
    let session: SessionId = id.parse()?;
    Ok(AggregateId::Session(session))
}

/// An elected store on a fresh in-memory database, plus one committed event so
/// every governed table has a row for `UPDATE`/`DELETE` to aim at. A test that
/// attacked an empty table could not tell a denial from a no-op.
fn seeded() -> Result<(EventStore, AggregateId), Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    let aggregate = session("ses_governed")?;
    store.elect_writer(&aggregate)?;
    let id: EventId = "evt_governed_seed".parse()?;
    let version = EventVersion::new(1).ok_or("version 1")?;
    let writer = store.fencing_token(&aggregate)?;
    store
        .append(
            AppendInput {
                id: &id,
                aggregate: aggregate.clone(),
                writer,
                kind_name: "session.created",
                version,
                created_ms: 1_786_060_800_000,
                data: &json!({ "sessionID": "ses_governed", "info": { "id": "ses_governed" } }),
            },
            &mut [],
        )?
        .1
        .discard();
    Ok((store, aggregate))
}

/// Asserts the statement was refused BY THE AUTHORIZER, naming the law.
fn assert_denied(outcome: Result<usize, rusqlite::Error>, law: &str) {
    match outcome {
        Err(rusqlite::Error::SqliteFailure(code, _))
            if code.code == ErrorCode::AuthorizationForStatementDenied => {}
        other => panic!("{law}: expected an authorizer denial, got {other:?}"),
    }
}

/// Runs one statement the way a consumer service would: inside the one
/// transaction the store hands out.
fn through_the_chokepoint(store: &mut EventStore, sql: &str) -> Result<usize, rusqlite::Error> {
    let tx = store.governed_transaction()?;
    let outcome = tx.execute(sql, []);
    // Roll back whatever a future regression let through, so a failing test
    // cannot leave a written row behind for the next assertion to read.
    drop(tx);
    outcome
}

#[test]
fn governed_schema_and_database_boundaries_cannot_be_removed() -> TestResult {
    for sql in [
        "DROP TABLE event",
        "DROP TRIGGER event_refuses_new_unfenced_write",
        "DROP INDEX event_aggregate_seq_idx",
        "ALTER TABLE event RENAME TO escaped_event",
        "CREATE TEMP TABLE EVENT (id TEXT)",
        "CREATE TEMP VIEW event AS SELECT * FROM main.event",
        "CREATE TEMP TRIGGER discard_fact BEFORE INSERT ON event BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER discard_fact BEFORE INSERT ON event BEGIN SELECT RAISE(IGNORE); END",
        "CREATE INDEX diverted_index ON event(id)",
        "REINDEX event_aggregate_seq_idx",
        "PRAGMA writable_schema=ON",
        "ATTACH ':memory:' AS escaped",
        "DETACH main",
    ] {
        let (store, _) = seeded()?;
        assert_denied(store.connection().execute(sql, []), sql);
    }
    let (store, _) = seeded()?;
    for (sql, value) in [("ATTACH ?1 AS escaped", ":memory:"), ("DETACH ?1", "main")] {
        assert_denied(store.connection().execute(sql, [value]), sql);
    }
    store.connection().execute_batch(
        "CREATE TABLE consumer (id TEXT); CREATE INDEX consumer_index ON consumer(id);
         INSERT INTO consumer VALUES ('retained'); REINDEX consumer_index;
         CREATE TEMP TABLE temporary_consumer (id TEXT);
         CREATE INDEX temp.event_aggregate_seq_idx ON temporary_consumer(id);
         REINDEX temp.event_aggregate_seq_idx;",
    )?;
    assert_denied(
        store
            .connection()
            .execute("ALTER TABLE temp.temporary_consumer RENAME TO event", []),
        "temporary rename cannot shadow the log",
    );
    assert_eq!(
        store
            .connection()
            .query_row("SELECT count(*) FROM event", [], |row| row.get::<_, i64>(0))?,
        1
    );
    Ok(())
}

#[test]
fn the_fact_log_refuses_a_consumer_insert() -> TestResult {
    let (mut store, _) = seeded()?;
    assert_denied(
        through_the_chokepoint(
            &mut store,
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
             VALUES ('evt_smuggled', 'session', 'ses_governed', 99, 0, 'session.created.1', '{}', 1)",
        ),
        "a consumer may not INSERT a durable fact",
    );
    Ok(())
}

#[test]
fn the_projection_watermark_refuses_a_consumer_write() -> TestResult {
    // F3B2's table joins the governed set. A consumer that could write it could
    // move a projection's durable position without moving the rows — inventing
    // the exact "leads its canonical input" state the law forbids, or hiding a
    // real lag by declaring the projection caught up.
    let (mut store, _) = seeded()?;
    assert_denied(
        through_the_chokepoint(
            &mut store,
            "INSERT INTO projection_watermark (projection, aggregate_family, aggregate_id, seq)
             VALUES ('roster', 'session', 'ses_governed', 99)",
        ),
        "a consumer may not INSERT projection progress",
    );
    assert_denied(
        through_the_chokepoint(&mut store, "UPDATE projection_watermark SET seq = 99"),
        "a consumer may not UPDATE projection progress",
    );
    assert_denied(
        through_the_chokepoint(&mut store, "DELETE FROM projection_watermark"),
        "a consumer may not DELETE projection progress",
    );
    Ok(())
}

#[test]
fn the_fact_log_refuses_a_consumer_update() -> TestResult {
    let (mut store, _) = seeded()?;
    // This exact statement is `F3B1A_RESIDUALS.md` item 2: the trigger covers
    // INSERT only, so before the chokepoint an UPDATE could set a committed
    // row's epoch back to NULL and launder a fenced fact into a legacy-shaped
    // one. There is no second trigger; the denial is what closes it.
    assert_denied(
        through_the_chokepoint(&mut store, "UPDATE event SET writer_epoch = NULL"),
        "a consumer may not unfence a committed fact by UPDATE",
    );
    Ok(())
}

#[test]
fn the_fact_log_refuses_a_consumer_delete() -> TestResult {
    let (mut store, _) = seeded()?;
    assert_denied(
        through_the_chokepoint(&mut store, "DELETE FROM event"),
        "a consumer may not DELETE a durable fact",
    );
    Ok(())
}

// The (table x verb) grid, one test per cell.
//
// Looping the three verbs inside one test per table would collapse the grid: a
// mutation that reopened exactly one verb, and a mutation that ungoverned the
// whole table, would produce the same failure record, and the manifest could no
// longer tell which law each edit removed. Twelve named cells keep every mutation
// attributable to one carrier.

#[test]
fn the_sequence_cursor_refuses_a_consumer_insert() -> TestResult {
    let (mut store, _) = seeded()?;
    assert_denied(
        through_the_chokepoint(
            &mut store,
            "INSERT INTO event_sequence (aggregate_family, aggregate_id, seq)
             VALUES ('session', 'ses_other', 7)",
        ),
        "a consumer may not INSERT a per-aggregate cursor",
    );
    Ok(())
}

#[test]
fn the_sequence_cursor_refuses_a_consumer_update() -> TestResult {
    let (mut store, _) = seeded()?;
    assert_denied(
        through_the_chokepoint(&mut store, "UPDATE event_sequence SET seq = 0"),
        "a consumer may not rewind a per-aggregate cursor",
    );
    Ok(())
}

#[test]
fn the_sequence_cursor_refuses_a_consumer_delete() -> TestResult {
    let (mut store, _) = seeded()?;
    assert_denied(
        through_the_chokepoint(&mut store, "DELETE FROM event_sequence"),
        "a consumer may not DELETE a per-aggregate cursor",
    );
    Ok(())
}

// The election row is what `fence` compares against. A consumer that could write
// it could name itself the elected writer, and every stale-writer refusal F3B1a
// proved would be reachable only by accident.

#[test]
fn the_election_row_refuses_a_consumer_insert() -> TestResult {
    let (mut store, _) = seeded()?;
    assert_denied(
        through_the_chokepoint(
            &mut store,
            "INSERT INTO aggregate_writer (aggregate_family, aggregate_id, owner_id, epoch)
             VALUES ('session', 'ses_seized', 'impostor', 1)",
        ),
        "a consumer may not INSERT an elected writer",
    );
    Ok(())
}

#[test]
fn the_election_row_refuses_a_consumer_update() -> TestResult {
    let (mut store, _) = seeded()?;
    assert_denied(
        through_the_chokepoint(
            &mut store,
            "UPDATE aggregate_writer SET owner_id = 'impostor'",
        ),
        "a consumer may not seize an elected writer by UPDATE",
    );
    Ok(())
}

#[test]
fn the_election_row_refuses_a_consumer_delete() -> TestResult {
    let (mut store, _) = seeded()?;
    assert_denied(
        through_the_chokepoint(&mut store, "DELETE FROM aggregate_writer"),
        "a consumer may not DELETE an elected writer",
    );
    Ok(())
}

// The database-format stamp decides whether this file is Rust-owned at all.

#[test]
fn the_engine_epoch_stamp_refuses_a_consumer_insert() -> TestResult {
    let (mut store, _) = seeded()?;
    assert_denied(
        through_the_chokepoint(
            &mut store,
            "INSERT INTO engine_epoch (epoch, engine, version, time) VALUES (1, 'ts', 'x', 0)",
        ),
        "a consumer may not INSERT a second ownership stamp",
    );
    Ok(())
}

#[test]
fn the_engine_epoch_stamp_refuses_a_consumer_update() -> TestResult {
    let (mut store, _) = seeded()?;
    assert_denied(
        through_the_chokepoint(&mut store, "UPDATE engine_epoch SET engine = 'typescript'"),
        "a consumer may not restamp database ownership",
    );
    Ok(())
}

#[test]
fn the_engine_epoch_stamp_refuses_a_consumer_delete() -> TestResult {
    let (mut store, _) = seeded()?;
    assert_denied(
        through_the_chokepoint(&mut store, "DELETE FROM engine_epoch"),
        "a consumer may not DELETE the ownership stamp",
    );
    Ok(())
}

#[test]
fn an_uppercase_statement_names_the_same_governed_table() -> TestResult {
    let (mut store, _) = seeded()?;
    // SQLite identifiers are case-insensitive, so this must be refused like any
    // other `DELETE FROM event`.
    //
    // What this does NOT prove: that the authorizer's own comparison is
    // case-insensitive. A mutation to `==` left this green, which measured the
    // reason — SQLite reports the name as the SCHEMA declares it, so the
    // statement's spelling never reaches the comparison. The test is kept because
    // the refusal is the behaviour that matters; the manifest deliberately
    // carries no row claiming the comparison, and `GovernedTable::from_name`
    // records it as unattributed defence in depth.
    assert_denied(
        through_the_chokepoint(&mut store, "DELETE FROM EVENT"),
        "the denial follows the table, not the statement's spelling",
    );
    Ok(())
}

#[test]
fn the_raw_read_seam_cannot_write_a_governed_table() -> TestResult {
    let (store, _) = seeded()?;
    // `EventStore::connection` survives F3B1b because reads and a consumer's own
    // tables are not the hazard. It is only safe because the denial rides on the
    // CONNECTION, not on the transaction type: no transaction is opened here at
    // all, and the write is still refused.
    assert_denied(
        store
            .connection()
            .execute("UPDATE event SET writer_epoch = NULL", []),
        "the read seam is not a write escape",
    );
    Ok(())
}

#[test]
fn a_consumers_own_table_is_untouched_by_the_denial() -> TestResult {
    let (mut store, _) = seeded()?;
    // The negative control. A chokepoint that refused everything would pass every
    // test above and make the store useless; the six services write their own
    // projection tables through this same transaction on every call.
    store
        .connection()
        .execute_batch("CREATE TABLE consumer_row (id TEXT PRIMARY KEY, note TEXT)")?;
    let tx = store.governed_transaction()?;
    tx.execute(
        "INSERT INTO consumer_row (id, note) VALUES ('r1', 'projected')",
        [],
    )?;
    tx.execute("UPDATE consumer_row SET note = 'updated'", [])?;
    tx.execute("DELETE FROM consumer_row", [])?;
    tx.commit()?.discard();
    Ok(())
}

/// A projector that tries to write the fact log it is being run for.
struct HostileProjector {
    refusal: Option<rusqlite::Error>,
}

impl Projector for HostileProjector {
    fn projection(&self) -> ProjectionId {
        ProjectionId::new("hostile")
    }

    fn project(
        &mut self,
        tx: &rusqlite::Transaction<'_>,
        _event: &DecodedEvent,
    ) -> Result<(), String> {
        // Deliberately swallowed: the assertion belongs to the test, and a
        // projector error would roll the append back and hide WHY it failed.
        self.refusal = tx.execute("UPDATE event SET writer_epoch = NULL", []).err();
        Ok(())
    }
}

#[test]
fn a_projector_runs_with_the_denial_on() -> TestResult {
    let (mut store, aggregate) = seeded()?;
    let id: EventId = "evt_projector_probe".parse()?;
    let version = EventVersion::new(1).ok_or("version 1")?;
    let writer = store.fencing_token(&aggregate)?;
    let mut hostile = HostileProjector { refusal: None };
    // A projector is foreign code running INSIDE the append transaction, between
    // the cursor write and the fact write. If `append_within` held one lift
    // across the whole append instead of one per statement, this is the window
    // that would be open.
    store
        .append(
            AppendInput {
                id: &id,
                aggregate,
                writer,
                kind_name: "session.created",
                version,
                created_ms: 1_786_060_800_001,
                data: &json!({ "sessionID": "ses_governed", "info": { "id": "ses_governed" } }),
            },
            &mut [&mut hostile],
        )?
        .1
        .discard();
    match hostile.refusal {
        Some(rusqlite::Error::SqliteFailure(code, _))
            if code.code == ErrorCode::AuthorizationForStatementDenied => {}
        other => panic!("a projector must run with the denial on, got {other:?}"),
    }
    Ok(())
}

#[test]
fn the_store_lowers_its_lift_before_returning_from_append() -> TestResult {
    // `seeded` already performed a successful append, which lifted the denial
    // twice — once for the cursor and once for the fact row. If either guard
    // failed to lower it, the gate would still be raised for `event` right now.
    let (mut store, _) = seeded()?;
    assert_denied(
        through_the_chokepoint(&mut store, "UPDATE event SET writer_epoch = NULL"),
        "a completed append must leave no open write window",
    );
    Ok(())
}

/// A projector that always fails, so the append aborts AFTER the cursor lift.
struct FailingProjector;

impl Projector for FailingProjector {
    fn projection(&self) -> ProjectionId {
        ProjectionId::new("failing")
    }

    fn project(
        &mut self,
        _tx: &rusqlite::Transaction<'_>,
        _event: &DecodedEvent,
    ) -> Result<(), String> {
        Err("refused on purpose".into())
    }
}

#[test]
fn a_failed_append_lowers_its_lift_too() -> TestResult {
    let (mut store, aggregate) = seeded()?;
    let id: EventId = "evt_failed_append".parse()?;
    let version = EventVersion::new(1).ok_or("version 1")?;
    let writer = store.fencing_token(&aggregate)?;
    let outcome = store.append(
        AppendInput {
            id: &id,
            aggregate,
            writer,
            kind_name: "session.created",
            version,
            created_ms: 1_786_060_800_002,
            data: &json!({ "sessionID": "ses_governed", "info": { "id": "ses_governed" } }),
        },
        &mut [&mut FailingProjector],
    );
    assert!(
        matches!(outcome, Err(WriteError::Projector { .. })),
        "the staged append must fail at the projector: {outcome:?}"
    );
    // The cursor lift was taken and then abandoned through `?`. An unwind is
    // exactly the path a hand-written "raise, do work, lower" pair skips.
    assert_denied(
        through_the_chokepoint(&mut store, "UPDATE event_sequence SET seq = 0"),
        "an aborted append must leave no open write window",
    );
    Ok(())
}

#[test]
fn the_governed_transaction_takes_the_write_lock_at_begin() -> TestResult {
    // `connection_mut` left the isolation level to each of the 29 call sites,
    // and all 29 chose IMMEDIATE. `governed_transaction` is now the only way to
    // open one, so IMMEDIATE has to be its behaviour rather than a convention
    // repeated correctly 29 times.
    //
    // Proven by consequence, not by reading the source: with the busy timeout at
    // zero, a second writer is refused the moment the first has BEGUN. A
    // DEFERRED transaction takes no write lock at BEGIN and the second BEGIN
    // would succeed.
    let dir = std::env::temp_dir().join(format!("grapt-store-immediate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("immediate.db");

    let mut first = EventStore::open(&db, "test")?;
    let mut second = EventStore::open(&db, "test")?;
    first.connection().busy_timeout(std::time::Duration::ZERO)?;
    second
        .connection()
        .busy_timeout(std::time::Duration::ZERO)?;

    let held = first.governed_transaction()?;
    match second.governed_transaction() {
        Err(rusqlite::Error::SqliteFailure(code, _))
            if code.code == ErrorCode::DatabaseBusy || code.code == ErrorCode::DatabaseLocked => {}
        other => panic!("BEGIN IMMEDIATE must take the write lock at BEGIN: {other:?}"),
    }
    drop(held);
    drop(first);
    drop(second);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// A file-backed store plus one committed fact, so a second connection has
/// something to attack.
fn seeded_on_disk(
    name: &str,
) -> Result<(std::path::PathBuf, EventStore), Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("grapt-store-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("governed.db");
    let mut store = EventStore::open(&db, "test")?;
    let aggregate = session("ses_foreign")?;
    store.elect_writer(&aggregate)?;
    let id: EventId = "evt_foreign_seed".parse()?;
    let version = EventVersion::new(1).ok_or("version 1")?;
    let writer = store.fencing_token(&aggregate)?;
    store
        .append(
            AppendInput {
                id: &id,
                aggregate,
                writer,
                kind_name: "session.created",
                version,
                created_ms: 1_786_060_800_000,
                data: &json!({ "sessionID": "ses_foreign", "info": { "id": "ses_foreign" } }),
            },
            &mut [],
        )?
        .1
        .discard();
    Ok((db, store))
}

#[test]
fn a_foreign_connection_is_outside_the_chokepoint() -> TestResult {
    // The exact limit of the guarantee, as an executable fact rather than a
    // sentence in a residuals file. A SQLite authorizer is installed on ONE
    // connection; a second connection to the same file has none, so it can do
    // what `the_fact_log_refuses_a_consumer_update` proves the store refuses.
    //
    // Recorded, not laundered: this is why `F3B1B_RESIDUALS.md` item 1 says the
    // UPDATE half of the law is connection-scoped, and it is what the two
    // consumer fixtures that stage hostile durable state now rely on.
    let (db, _store) = seeded_on_disk("foreign-update")?;
    let affected =
        rusqlite::Connection::open(&db)?.execute("UPDATE event SET writer_epoch = NULL", [])?;
    assert_eq!(
        affected, 1,
        "a connection outside the store carries no denial; if this ever refuses, the chokepoint \
         became database-wide and the residual should be closed rather than re-stated"
    );
    let _ = std::fs::remove_dir_all(db.parent().unwrap_or(&db));
    Ok(())
}

#[test]
fn a_new_unfenced_row_is_refused_from_a_foreign_connection_too() -> TestResult {
    // The other half, and the reason the previous test is a bounded disclosure
    // rather than a hole. The no-new-v1 gate is a TRIGGER, which lives in the
    // schema, so it fires for every connection to the file — including the one
    // that just proved it can UPDATE.
    //
    // WHAT THIS PROVES, EXACTLY: while that trigger exists, a new unfenced row is
    // refused from a connection the store does not own. It does NOT prove the row
    // can never be written: a foreign connection can remove the trigger.
    // The store's own connection refuses that removal through its authorizer.
    let (db, _store) = seeded_on_disk("foreign-insert")?;
    let outcome = rusqlite::Connection::open(&db)?.execute(
        "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
         VALUES ('evt_foreign_v1', 'session', 'ses_foreign', 99, 0, 'session.created.1', '{}', NULL)",
        [],
    );
    match outcome {
        Err(rusqlite::Error::SqliteFailure(_, Some(ref message)))
            if message.contains("no new v1 write") => {}
        other => panic!(
            "the schema trigger must refuse a new unfenced row from any connection: {other:?}"
        ),
    }
    let _ = std::fs::remove_dir_all(db.parent().unwrap_or(&db));
    Ok(())
}

#[test]
fn election_and_append_still_reach_the_database_through_the_chokepoint() -> TestResult {
    // The last negative control, and the broadest one: every denial above is
    // worthless if the store's own two governed writers stopped working. This
    // reads back what `elect_writer` and `append` actually committed.
    let (store, _) = seeded()?;
    let elected: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM aggregate_writer WHERE aggregate_id = 'ses_governed'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(elected, 1, "the election row must be written by the store");
    let facts: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))?;
    assert_eq!(facts, 1, "the fact row must be written by the store");
    let cursor: i64 = store.connection().query_row(
        "SELECT seq FROM event_sequence WHERE aggregate_id = 'ses_governed'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(cursor, 0, "the cursor must be written by the store");
    Ok(())
}

#[test]
fn the_plan_read_table_refuses_a_consumer_write() -> TestResult {
    // F4B2a's table joins the governed set. A recorded plan read is durable
    // OPERATIONAL truth — the audit trail of which verified plan bytes gated
    // an attempt — so a consumer that could write it could forge or erase
    // that trail without ever touching the sealed receipt types.
    let (mut store, _) = seeded()?;
    assert_denied(
        through_the_chokepoint(
            &mut store,
            "INSERT INTO plan_read_receipt (receipt_id, campaign_id, recorded, record_bytes)
             VALUES ('read-forged', 'cmp-forged', 0, x'00')",
        ),
        "a consumer may not INSERT durable plan-read truth",
    );
    assert_denied(
        through_the_chokepoint(
            &mut store,
            "UPDATE plan_read_receipt SET record_bytes = x'00'",
        ),
        "a consumer may not rewrite recorded plan-read truth",
    );
    assert_denied(
        through_the_chokepoint(&mut store, "DELETE FROM plan_read_receipt"),
        "a consumer may not DELETE recorded plan-read truth",
    );
    Ok(())
}

#[test]
fn the_attempt_receipt_table_refuses_a_consumer_write() -> TestResult {
    // R2C5A1's table joins the governed set, and the consequence of a consumer
    // write here is the sharpest of the three operational tables. The receipt is
    // the ONLY crash-visible evidence that a send-capable boundary was ever
    // permitted — no durable record can establish that bytes actually left, and
    // this one does not claim to — and its markers are what a reconciler
    // classifies from:
    //
    // * an INSERT forges an attempt nobody made;
    // * an UPDATE setting `settled_ms` turns an AMBIGUOUS lost attempt into a
    //   settled one, which authorises the automatic replay of an external effect
    //   the law forbids replaying;
    // * a DELETE erases the evidence outright, and an absent receipt reads as
    //   "no external call was ever made".
    let (mut store, _) = seeded()?;
    assert_denied(
        through_the_chokepoint(
            &mut store,
            "INSERT INTO attempt_receipt
                (attempt_id, session_id, step, ordinal, writer_epoch,
                 reserved_ms, request_sent_ms, settled_ms, record_bytes)
             VALUES ('att-forged', 'ses_governed', 1, 1, 1, 0, NULL, NULL, x'00')",
        ),
        "a consumer may not INSERT a durable attempt receipt",
    );
    assert_denied(
        through_the_chokepoint(&mut store, "UPDATE attempt_receipt SET settled_ms = 1"),
        "a consumer may not settle an attempt receipt, which would make an \
         ambiguous external effect look replayable",
    );
    assert_denied(
        through_the_chokepoint(&mut store, "DELETE FROM attempt_receipt"),
        "a consumer may not DELETE process-loss evidence",
    );
    Ok(())
}
