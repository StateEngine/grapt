use super::*;
use grapt_event::session_message::{
    AssistantMessageOpenedFact, ContentBlockKind, ContentBlockOpenedFact, UsageObservedFact,
};
use grapt_types::{AttemptUsage, TokenCount, UsageField, UsageObservationPhase};
use rusqlite::Connection;
mod compaction;
mod inherited;
mod seat_day;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Fixture {
    connection: Connection,
    path: std::path::PathBuf,
}
impl std::ops::Deref for Fixture {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.connection
    }
}
impl Fixture {
    fn new(name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let path =
            std::env::temp_dir().join(format!("lua-history-{name}-{}.sqlite", std::process::id()));
        if path.exists() {
            return Err("fixture already exists; preserve evidence".into());
        }
        let connection = Connection::open(&path)?;
        Ok(Self { connection, path })
    }
    fn read(&self, session: &str) -> Result<AccountingSnapshot, String> {
        let log = grapt_store::EventLog::open_read_only(&self.path).map_err(|e| e.to_string())?;
        log.with_read_snapshot(|snapshot| {
            SessionHistoryStore::accounting_snapshot(
                snapshot,
                &session.parse().map_err(|_| "session")?,
            )
        })
    }
    fn finish(self) -> TestResult {
        drop(self.connection);
        std::fs::remove_file(self.path)?;
        Ok(())
    }
}

fn schema(conn: &Connection) -> TestResult {
    conn.execute_batch("CREATE TABLE event(id TEXT, aggregate_family TEXT, aggregate_id TEXT, seq INTEGER, created INTEGER, type TEXT, data TEXT); CREATE UNIQUE INDEX event_order ON event(aggregate_family,aggregate_id,seq);")?;
    Ok(())
}

fn row(conn: &Connection, session: &str, seq: i64, kind: &str, data: &Value) -> TestResult {
    raw(conn, session, seq, kind, &serde_json::to_string(data)?)
}

fn raw(conn: &Connection, session: &str, seq: i64, kind: &str, data: &str) -> TestResult {
    conn.execute(
        "INSERT INTO event VALUES(?1,'session',?2,?3,?4,?5,?6)",
        rusqlite::params![
            format!("evt_{session}_{seq}"),
            session,
            seq,
            100 + seq,
            kind,
            data
        ],
    )?;
    Ok(())
}

fn open(conn: &Connection, session: &str, seq: i64, attempt: &str, step: u32) -> TestResult {
    let fact = AssistantMessageOpenedFact::new(
        PhysicalAttemptId::new(attempt).ok_or("attempt")?,
        StepNumber::new(step).ok_or("step")?,
        "msg_accounting".parse()?,
    );
    row(
        conn,
        session,
        seq,
        "session.assistant-message.opened.1",
        &session_message::encode_opened(&fact),
    )
}

fn usage(conn: &Connection, session: &str, seq: i64, attempt: &str, count: u64) -> TestResult {
    let field = UsageField::reported(TokenCount::new(count)?);
    let usage = AttemptUsage::observed(
        UsageField::ABSENT,
        field.clone(),
        field,
        UsageField::ABSENT,
        UsageField::ABSENT,
        UsageField::ABSENT,
        UsageField::ABSENT,
        UsageObservationPhase::Terminal,
    );
    let fact = UsageObservedFact::new(PhysicalAttemptId::new(attempt).ok_or("attempt")?, usage);
    row(
        conn,
        session,
        seq,
        "session.usage.observed.2",
        &session_message::encode_observed(&fact),
    )
}

#[test]
fn lua_snapshot_joins_retries_preserves_unknowns_and_reconciles_once() -> TestResult {
    let conn = Fixture::new("joins")?;
    schema(&conn)?;
    // The first attempt carries a real model, so a retry inheriting it is
    // observable; a selection-less opening would pass that check either way.
    let selected = AssistantMessageOpenedFact::new(
        PhysicalAttemptId::new("pat_first").ok_or("attempt")?,
        StepNumber::new(1).ok_or("step")?,
        "msg_accounting".parse()?,
    )
    .with_selection(session_message::AssistantSelection::new(
        grapt_types::AgentId::new("fixture").ok_or("agent")?,
        grapt_types::ProviderId::new("fixture").ok_or("provider")?,
        grapt_types::ModelId::new("fixture-model").ok_or("model")?,
        None,
    ));
    row(
        &conn,
        "ses_accounting",
        0,
        "session.assistant-message.opened.2",
        &session_message::encode_opened(&selected),
    )?;
    usage(&conn, "ses_accounting", 1, "pat_first", 7)?;
    usage(&conn, "ses_accounting", 2, "pat_first", 7)?;
    let block = ContentBlockOpenedFact::new(
        PhysicalAttemptId::new("pat_retry").ok_or("attempt")?,
        "msg_accounting".parse()?,
        0,
        ContentBlockKind::Text,
    );
    row(
        &conn,
        "ses_accounting",
        3,
        "session.content-block.opened.1",
        &session_message::encode_block_opened(&block),
    )?;
    usage(&conn, "ses_accounting", 4, "pat_retry", 11)?;
    usage(&conn, "ses_accounting", 5, "pat_unbound", 13)?;
    usage(&conn, "ses_foreign", 0, "pat_first", 999)?;
    let report = conn.read("ses_accounting")?;
    assert_eq!(report.attempts.len(), 3);
    let first = &report.attempts[0];
    assert_eq!(
        first.usage.input_fresh().map(|f| f.count().value()),
        Some(7),
        "duplicate charging"
    );
    assert_eq!(
        first.selection.as_ref().map(|s| s.model().as_str()),
        Some("fixture-model"),
        "exact attempt lost its model"
    );
    let retry = &report.attempts[1];
    assert_eq!(
        retry.step.map(StepNumber::value),
        Some(1),
        "wrong retry attempt join"
    );
    assert!(retry.selection.is_none(), "retry inherited unproven model");
    assert!(retry.opening.is_none(), "retry invented opening timestamp");
    assert_eq!(retry.first_block.as_ref().map(|p| p.created_ms), Some(103));
    assert!(
        report.attempts[2].step.is_none(),
        "unbound usage inherited latest Step"
    );
    assert!(
        first.usage.reasoning().is_none(),
        "absent usage became zero"
    );
    assert!(
        first.step_terminal.is_none(),
        "interrupted attempt became complete"
    );
    assert!(report.enumeration_complete);
    conn.finish()
}

#[test]
fn lua_snapshot_skips_what_the_owner_skips_and_refuses_owned_corruption() -> TestResult {
    let conn = Fixture::new("eras")?;
    schema(&conn)?;
    for session in ["ses_plain", "ses_eras"] {
        open(&conn, session, 0, "pat_first", 1)?;
        usage(&conn, session, 1, "pat_first", 7)?;
    }
    // The owner fold skips other eras of owned names and unowned kinds that
    // carry no numeric version; migrated histories legitimately hold them.
    let empty = serde_json::json!({});
    row(&conn, "ses_eras", 2, "session.step.ended.1", &empty)?;
    row(&conn, "ses_eras", 3, "session.step.failed.1", &empty)?;
    row(&conn, "ses_eras", 4, "session.content.appended.9", &empty)?;
    raw(&conn, "ses_eras", 5, "marker", "not json")?;
    raw(&conn, "ses_eras", 6, "workspace.note", "not json")?;
    row(
        &conn,
        "ses_eras",
        7,
        "session.assistant-message.opened.9",
        &empty,
    )?;
    row(&conn, "ses_eras", 8, "session.usage.observed.9", &empty)?;
    let plain = conn.read("ses_plain")?;
    let eras = conn
        .read("ses_eras")
        .map_err(|e| format!("owner-skipped row refused: {e}"))?;
    let shape = |snapshot: &AccountingSnapshot| {
        snapshot
            .attempts
            .iter()
            .map(|attempt| {
                (
                    attempt.attempt.as_str().to_owned(),
                    attempt.step.map(StepNumber::value),
                    attempt.usage.input_fresh().map(|f| f.count().value()),
                    attempt.step_terminal.is_some(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        shape(&plain),
        shape(&eras),
        "owner-skipped row changed the attempt set"
    );
    assert_eq!(eras.events_read, 9);
    raw(&conn, "ses_unversioned", 0, "session.step.ended", "{}")?;
    let refusal = conn
        .read("ses_unversioned")
        .err()
        .ok_or("unversioned owned row skipped")?;
    assert!(
        refusal.contains("seq 0 (session.step.ended)"),
        "refusal lost its row: {refusal}"
    );
    raw(
        &conn,
        "ses_not_json",
        3,
        "session.content.appended.1",
        "not json",
    )?;
    let refusal = conn
        .read("ses_not_json")
        .err()
        .ok_or("corrupt owned row skipped")?;
    assert!(
        refusal.contains("seq 3 (session.content.appended)"),
        "refusal lost its row: {refusal}"
    );
    conn.finish()
}

#[test]
fn lua_snapshot_bounds_and_malformed_rows_are_visible() -> TestResult {
    let conn = Fixture::new("bounds")?;
    schema(&conn)?;
    conn.execute_batch("BEGIN;")?;
    for seq in 0..=MAX_ACCOUNTING_ROWS {
        row(
            &conn,
            "ses_bound",
            seq as i64,
            "session.created.1",
            &serde_json::json!({}),
        )?;
    }
    conn.execute_batch("COMMIT;")?;
    let snapshot = conn.read("ses_bound")?;
    assert!(
        !snapshot.enumeration_complete,
        "bounded pagination claimed exhaustive history"
    );
    assert_eq!(snapshot.events_read, MAX_ACCOUNTING_ROWS);
    row(
        &conn,
        "ses_corrupt",
        0,
        "session.usage.observed.2",
        &serde_json::json!({}),
    )?;
    assert!(conn.read("ses_corrupt").is_err());
    assert!(conn.read("ses_absent").is_err());
    raw(
        &conn,
        "ses_negative",
        i64::MIN,
        "session.usage.observed.2",
        "{}",
    )?;
    let refusal = conn
        .read("ses_negative")
        .err()
        .ok_or("first page skipped a negative sequence")?;
    assert!(
        refusal.contains(&format!("the stored sequence is {}", i64::MIN)),
        "{refusal}"
    );
    conn.finish()
}

#[test]
fn lua_snapshot_refuses_cross_message_attempt_and_oversized_data() -> TestResult {
    let conn = Fixture::new("conflict")?;
    schema(&conn)?;
    open(&conn, "ses_conflict", 0, "pat_same", 1)?;
    let fact = AssistantMessageOpenedFact::new(
        PhysicalAttemptId::new("pat_same").ok_or("attempt")?,
        StepNumber::new(2).ok_or("step")?,
        "msg_other".parse()?,
    );
    row(
        &conn,
        "ses_conflict",
        1,
        "session.assistant-message.opened.1",
        &session_message::encode_opened(&fact),
    )?;
    row(
        &conn,
        "ses_large",
        0,
        "session.created.1",
        &Value::String("x".repeat(ROW_BYTES)),
    )?;
    assert!(
        conn.read("ses_conflict").is_err(),
        "wrong attempt join survived"
    );
    assert!(conn.read("ses_large").is_err());
    conn.finish()
}

#[test]
fn lua_snapshot_concurrent_append_and_restart_keep_one_read_boundary() -> TestResult {
    let conn = Fixture::new("concurrent")?;
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    schema(&conn)?;
    open(&conn, "ses_snapshot", 0, "pat_snapshot", 1)?;
    for seq in 1..140 {
        row(
            &conn,
            "ses_snapshot",
            seq,
            "session.created.1",
            &serde_json::json!({}),
        )?;
    }
    let session = "ses_snapshot".parse()?;
    {
        let log = grapt_store::EventLog::open_read_only(&conn.path)?;
        let interrupted: Result<(), String> = log.with_read_snapshot(|snapshot| {
            let before = SessionHistoryStore::accounting_snapshot(snapshot, &session)?;
            usage(&conn, "ses_snapshot", 140, "pat_snapshot", 17).map_err(|e| e.to_string())?;
            let during = SessionHistoryStore::accounting_snapshot(snapshot, &session)?;
            assert_eq!(
                before.last_seq, during.last_seq,
                "concurrent append crossed snapshot"
            );
            assert!(during.attempts[0].usage.input_fresh().is_none());
            Err("interrupted reader".into())
        });
        assert_eq!(interrupted, Err("interrupted reader".into()));
    }
    let restarted = conn.read("ses_snapshot")?;
    assert_eq!(restarted.last_seq, Some(140));
    assert_eq!(
        restarted.attempts[0]
            .usage
            .input_fresh()
            .map(|v| v.count().value()),
        Some(17)
    );
    conn.finish()
}

#[test]
fn lua_snapshot_attempt_observation_and_total_byte_budgets_refuse() -> TestResult {
    for (session, count, distinct) in [
        ("ses_observations", 65, false),
        ("ses_attempts", MAX_ACCOUNTING_ATTEMPTS + 1, true),
    ] {
        let conn = Fixture::new(session)?;
        schema(&conn)?;
        conn.execute_batch("BEGIN;")?;
        for seq in 0..count {
            let attempt = if distinct {
                format!("pat_{seq}")
            } else {
                "pat_same".to_owned()
            };
            usage(&conn, session, i64::try_from(seq)?, &attempt, 1)?;
        }
        conn.execute_batch("COMMIT;")?;
        let refusal = conn.read(session).err().ok_or("bound survived")?;
        assert!(
            refusal.contains(if distinct {
                "8192 attempts"
            } else {
                "64 usage observations"
            }),
            "wrong refusal: {refusal}"
        );
        conn.finish()?;
    }
    let conn = Fixture::new("bytes")?;
    schema(&conn)?;
    conn.execute_batch("BEGIN;")?;
    for seq in 0..=MAX_ACCOUNTING_BYTES / ROW_BYTES {
        row(
            &conn,
            "ses_bytes",
            i64::try_from(seq)?,
            "session.created.1",
            &Value::String("x".repeat(ROW_BYTES - 2)),
        )?;
    }
    conn.execute_batch("COMMIT;")?;
    let refusal = conn.read("ses_bytes").err().ok_or("byte budget survived")?;
    assert!(refusal.contains("64 MiB"), "wrong refusal: {refusal}");
    conn.finish()
}

mod request_cache;
