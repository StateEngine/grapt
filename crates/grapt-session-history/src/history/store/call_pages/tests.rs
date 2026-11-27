//! Observe the actual raw-data ownership boundary, including discarded probes.
use super::*;
use grapt_event::session_message::{encode_appended, ContentAppendedFact};
use grapt_types::{
    ArtifactDigest, ArtifactId, ArtifactRef, CaptureDisposition, ContentOrdinal, MessageId,
    NormalizedContentItem,
};
use rusqlite::backup::{Backup, StepResult};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn probe(padding: usize, count: usize, copied_rows: usize, fallback: bool) -> TestResult {
    let source = grapt_store::EventStore::open_in_memory()?;
    let mut conn = rusqlite::Connection::open_in_memory()?;
    let backup = Backup::new(source.connection(), &mut conn)?;
    if !matches!(backup.step(4096)?, StepResult::Done) {
        return Err("empty canonical schema exceeded bounded backup".into());
    }
    drop(backup);
    let session: SessionId = "ses_precopy_calls".parse()?;
    let call = ToolCallId::new("call_precopy_0").ok_or("call identity")?;
    let fact = ContentAppendedFact::new(
        "msg_precopy_calls".parse::<MessageId>()?,
        NormalizedContentItem::new(
            ContentOrdinal::new(0),
            ContentKind::Tool { call: call.clone() },
            ArtifactRef::new(
                ArtifactId::new("art_precopy").ok_or("artifact identity")?,
                1,
                ArtifactDigest::new("sha256:aa").ok_or("digest")?,
            ),
        ),
        CaptureDisposition::Complete,
    )?;
    let mut value = encode_appended(&fact);
    value["padding"] = "x".repeat(padding).into();
    conn.execute("INSERT INTO event_sequence (aggregate_family, aggregate_id, seq) VALUES ('session', ?1, 0)", [session.as_str()])?;
    let mut targets = Vec::new();
    let mut expected_copied = 0usize;
    for seq in 1..=count {
        let target = ToolCallId::new(format!("call_precopy_{seq}")).ok_or("call identity")?;
        value["item"]["call"] = target.as_str().into();
        let data = value.to_string();
        if seq <= copied_rows {
            expected_copied += data.len();
        }
        targets.push(target);
        let seq = i64::try_from(seq)?;
        conn.execute("INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch) VALUES (?1, 'session', ?2, ?3, 0, 'session.content.appended.1', ?4, 1)",
            (format!("evt_precopy_{seq}"), session.as_str(), seq, &data))?;
    }
    COPIED_DATA_BYTES.with(|bytes| bytes.set(0));
    let result = read(&conn, &session, &targets)?;
    let copied = COPIED_DATA_BYTES.with(|bytes| bytes.replace(0));
    assert_eq!(result.is_none(), fallback);
    assert_eq!(
        copied, expected_copied,
        "BR-H call probe copied data before its budget check"
    );
    assert!(copied <= SHARED_DATA_BYTES);
    Ok(())
}

#[test]
fn br_h_call_probe_refuses_one_oversized_row_before_copying() -> TestResult {
    probe(600 * 1024, 1, 0, true)
}

#[test]
fn br_h_call_probe_stops_cumulative_copies_before_the_budget() -> TestResult {
    // Two targets fit; the third target must not be copied even temporarily.
    probe(200 * 1024, 2, 2, false)?;
    probe(200 * 1024, 3, 2, true)
}

#[test]
fn br_h_call_step_failure_stays_unreadable() -> TestResult {
    let conn = rusqlite::Connection::open_in_memory()?;
    // Preparation succeeds; SQLite raises integer overflow when stepping data.
    conn.execute_batch("CREATE VIEW event AS SELECT 'evt_step' AS id, 'session' AS aggregate_family, 'ses_step' AS aggregate_id, 1 AS seq, 0 AS created, 'session.content.appended.1' AS type, abs(-9223372036854775808) AS data")?;
    let session: SessionId = "ses_step".parse()?;
    let call = ToolCallId::new("call_step").ok_or("call")?;
    let error = read(&conn, &session, &[call])
        .err()
        .ok_or("SQLite step failure became fallback or absence")?;
    assert!(matches!(error, HistoryReadError::Store { .. }));
    assert!(error.to_string().contains("integer overflow"));
    Ok(())
}
