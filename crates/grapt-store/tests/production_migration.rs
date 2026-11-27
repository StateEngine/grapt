//! F3B4c: the production migration, from outside the crate.
//!
//! Everything here runs against the PUBLIC surface — `EventStore::migrate`,
//! then a REAL `EventStore::open` on the migrated file. That second step is the
//! point. `migration::apply` opens its own connection and its own transaction
//! and never calls `ensure_schema`, which is deliberate — it is what keeps the
//! migration entry point reachable after `ensure_schema` starts refusing
//! old-shape opens. The consequence is that a migration returning `Applied`
//! proves only that its statements ran. The evidence has to be an open, because
//! only an open runs `verify_columns` four times, the four-arm target-shape
//! probe and the index column-shape verification.

use grapt_store::{
    AggregateId, AttemptRecordCeiling, CompactionAttempt, EventStore, MailboxPageEntry,
    MigrationOutcome, Projector, ReserveInput, TerminalCoordinates, WriteError,
};
use grapt_types::{
    ArtifactDigest, ArtifactId, ArtifactRef, AttemptOrdinal, BoundedAttemptId, BoundedSessionId,
    ClockAuthority, CompactionDescriptor, CompactionDescriptorParts, CompactionId,
    CompactionSummary, CompactionTrigger, ContextEpoch, ContextWindowId, DurableAttemptReceipt,
    EventId, LostEffect, MailboxBody, MailboxCursor, MailboxMessageId, MailboxSourceCursor,
    NextRequestRef, PhysicalAttemptId, ProtocolVersion, ReceiptDisposition, RequestSentMarker,
    ReservedMarker, RetainedHistory, RetentionPolicyVersion, SeatMailboxMessage, SeatMessageKind,
    SeatRecipientId, SessionId, StepNumber, Timestamp,
};
use rusqlite::Connection;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A clock whose reading is a known constant, so a durable journal timestamp
/// can be asserted rather than merely observed to be "recent".
#[derive(Debug)]
struct FrozenClock(i64);

impl ClockAuthority for FrozenClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

const FROZEN_MS: i64 = 1_786_060_800_123;

fn scratch(name: &str) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("grapt-f3b4c-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// The EXACT oracle-era shape, transcribed from the frozen sanitized clone.
///
/// Measured read-only on the 2026-08-08 sanitized clone:
/// `data` is `TEXT NOT NULL`, `event_sequence`'s primary key is the single
/// column `aggregate_id`, the foreign key is single-column with `ON DELETE
/// CASCADE`, and both indexes carry the target NAMES over family-BLIND column
/// lists. Every one of those is a reason the additive route cannot reach the
/// target shape.
const TYPESCRIPT_ERA_SCHEMA: &str = "CREATE TABLE `event_sequence` (
    `aggregate_id` text PRIMARY KEY,
    `seq` integer NOT NULL,
    `owner_id` text
);
CREATE TABLE `event` (
    `id` text PRIMARY KEY,
    `aggregate_id` text NOT NULL,
    `seq` integer NOT NULL,
    `created` integer NOT NULL,
    `type` text NOT NULL,
    `data` text NOT NULL,
    CONSTRAINT fk_event_aggregate_id_event_sequence_aggregate_id_fk
        FOREIGN KEY (`aggregate_id`) REFERENCES `event_sequence`(`aggregate_id`)
        ON DELETE CASCADE
);
CREATE UNIQUE INDEX `event_aggregate_seq_idx` ON `event` (`aggregate_id`,`seq`);
CREATE INDEX `event_aggregate_type_seq_idx` ON `event` (`aggregate_id`,`type`,`seq`);";

/// One session aggregate carrying a GAP, because the real corpus has them:
/// within-aggregate sequences are zero-based and strictly increasing but NOT
/// contiguous (a measured `1053 -> 1075` jump). A migration that compacted
/// them would renumber history.
fn typescript_era_database(path: &std::path::Path) -> TestResult {
    let conn = Connection::open(path)?;
    conn.execute_batch(TYPESCRIPT_ERA_SCHEMA)?;
    conn.execute_batch(
        "INSERT INTO event_sequence (aggregate_id, seq, owner_id) VALUES ('ses_alpha', 9, NULL);
         INSERT INTO event (id, aggregate_id, seq, created, type, data) VALUES
            ('evt_1','ses_alpha',0,1700000000000,'session.created.1','{\"directory\":\"E:/w\"}'),
            ('evt_2','ses_alpha',4,1700000000001,'session.updated.1','{ \"note\" : 1 }'),
            ('evt_3','ses_alpha',9,1700000000002,'session.updated.1','{\"z\":1,\"a\":2}');",
    )?;
    // oracle-era ownership marker, exactly as `epoch.rs` expects to find it.
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    assert_eq!(version, 0, "a TypeScript-era database is user_version 0");
    Ok(())
}

/// The rows as the source holds them, for a byte-level before/after comparison.
fn legacy_rows(conn: &Connection) -> Result<Vec<(String, i64, String, String)>, rusqlite::Error> {
    let mut statement =
        conn.prepare("SELECT id, seq, type, data FROM event ORDER BY aggregate_id, seq")?;
    let rows = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    rows.collect()
}

fn index_columns(conn: &Connection, index: &str) -> Result<Vec<String>, rusqlite::Error> {
    let mut statement = conn.prepare(&format!("PRAGMA index_info(\"{index}\")"))?;
    let rows = statement.query_map([], |r| r.get::<_, String>(2))?;
    rows.collect()
}

fn index_is_unique(conn: &Connection, index: &str) -> Result<bool, rusqlite::Error> {
    let mut statement = conn.prepare("PRAGMA index_list(\"event\")")?;
    let rows = statement
        .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .find(|(name, _)| name == index)
        .map(|(_, unique)| unique != 0)
        .unwrap_or(false))
}

/// Runs the production migration set, turning a refusal into a NAMED panic.
///
/// A test that returns `Err` prints `Error: ...`, never `panicked at`, and the
/// mutation runner reads the panic line for the assertion that fired. An
/// Err-returning witness therefore reds as "failed without a captured
/// diagnostic" — and two rows whose witnesses fail that way share one evidence
/// record, which the duplicate-evidence guard refuses. Naming the refusal keeps
/// each row's evidence its own and says WHICH law broke.
fn migrated(db: &std::path::Path, dir: &std::path::Path) -> MigrationOutcome {
    match EventStore::migrate_with_clock(db, dir, "0.0.1", &FrozenClock(FROZEN_MS)) {
        Ok(outcome) => outcome,
        Err(error) => panic!("the production migration refused: {error}"),
    }
}

/// Opens the migrated database, turning a refusal into a NAMED panic, for the
/// same reason.
fn opened(db: &std::path::Path) -> EventStore {
    match EventStore::open(db, "0.0.1") {
        Ok(store) => store,
        Err(error) => panic!("the migrated database refused to open: {error}"),
    }
}

include!("production_migration/typescript_rebuild.rs");
include!("production_migration/operational_composition.rs");

/// Every row the retired campaign draft feature could leave, read byte-exact.
const RETIRED_ROWS: [&str; 7] = [
    "SELECT id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch,
            envelope_bytes, campaign_transition_id FROM event
      WHERE aggregate_family = 'campaign' ORDER BY seq",
    "SELECT * FROM event_sequence WHERE aggregate_family = 'campaign'",
    "SELECT * FROM aggregate_writer WHERE aggregate_family = 'campaign'",
    "SELECT * FROM projection_watermark WHERE aggregate_family = 'campaign'",
    "SELECT * FROM campaign_phase",
    "SELECT * FROM campaign_plan_artifact",
    "SELECT * FROM plan_read_receipt",
];

fn retired_rows(db: &std::path::Path) -> Result<Vec<Vec<rusqlite::types::Value>>, rusqlite::Error> {
    let conn = Connection::open(db)?;
    let mut all = Vec::new();
    for sql in RETIRED_ROWS {
        let mut statement = conn.prepare(sql)?;
        let width = statement.column_count();
        let rows = statement.query_map([], |row| (0..width).map(|i| row.get(i)).collect())?;
        all.push(vec![rusqlite::types::Value::Text(sql.to_owned())]);
        all.extend(rows.collect::<Result<Vec<_>, _>>()?);
    }
    Ok(all)
}

fn record_terminal(store: &mut EventStore, session: &SessionId, id: &str) -> TestResult {
    let aggregate = AggregateId::Session(session.clone());
    store.elect_writer(&aggregate)?;
    let writer = store.fencing_token(&aggregate)?;
    let fact = grapt_event::session_tool_terminal::ToolTerminalFact::new(
        PhysicalAttemptId::new("att_retired_mix").ok_or("attempt")?,
        grapt_types::ToolCallId::new(id).ok_or("call")?,
        grapt_types::ToolTerminalOutcome::ResultMissing,
    );
    let tx = store.governed_transaction()?;
    grapt_store::tool_terminal::record(
        &tx,
        session,
        &id.parse()?,
        writer,
        FROZEN_MS,
        &fact,
        &mut [],
    )?;
    tx.commit()?.discard();
    Ok(())
}

/// Campaign drafting is gone, but its rows are user data: a database holding
/// them beside a live session migrates, opens and takes a fenced session write
/// with every retired row byte-identical.
#[test]
fn retired_campaign_rows_survive_migration_beside_a_live_session() -> TestResult {
    let dir = scratch("retired-campaign")?;
    let db = dir.join("grapt.db");
    let session: SessionId = "ses_retired_mix".parse()?;
    record_terminal(&mut opened(&db), &session, "evt_retired_mix_before")?;
    Connection::open(&db)?.execute_batch(
        "INSERT INTO event_sequence VALUES ('campaign', 'cmp_retired', 1, 'owner-retired');
         INSERT INTO aggregate_writer VALUES ('campaign', 'cmp_retired', 'owner-retired', 3);
         INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data,
                            writer_epoch, envelope_bytes, campaign_transition_id) VALUES
           ('evt_retired_0', 'campaign', 'cmp_retired', 0, 7, 'campaign.opened.1', NULL, 3,
            X'00FF7B22', 'ctr_retired_0'),
           ('evt_retired_1', 'campaign', 'cmp_retired', 1, 8, 'campaign.phase.1', NULL, 3,
            X'C3A9', 'ctr_retired_1');
         INSERT INTO projection_watermark VALUES ('campaign-phase', 'campaign', 'cmp_retired', 1);
         INSERT INTO campaign_phase VALUES ('cmp_retired', 'drafting');
         INSERT INTO campaign_plan_artifact VALUES
           ('cmp_retired', 'v1', 'art_retired', 12, 'digest-a', 'graph-b', 1);
         INSERT INTO plan_read_receipt VALUES ('rcp_retired', 'cmp_retired', 9, X'01020304');",
    )?;
    let before = retired_rows(&db)?;
    assert_eq!(
        before.len(),
        7 + 2 + 1 + 1 + 1 + 1 + 1 + 1,
        "every retired row is staged"
    );
    downgrade_event_tables(&db)?;
    assert!(matches!(
        migrated(&db, &dir),
        MigrationOutcome::Applied { .. }
    ));
    let mut store = opened(&db);
    record_terminal(&mut store, &session, "evt_retired_mix_after")?;
    let session_rows: Vec<i64> = {
        let conn = Connection::open(&db)?;
        let mut statement = conn.prepare(
            "SELECT seq FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
              AND type = 'session.tool.terminal.1' ORDER BY seq",
        )?;
        let rows = statement.query_map([session.as_str()], |row| row.get(0))?;
        rows.collect::<Result<_, _>>()?
    };
    assert_eq!(session_rows.len(), 2, "{session_rows:?}");
    drop(store);
    assert_eq!(retired_rows(&db)?, before);
    Ok(())
}
