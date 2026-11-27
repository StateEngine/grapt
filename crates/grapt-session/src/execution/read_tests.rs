//! BR execution-reader discriminator: real bundled schema, no timing assertions.
use super::{read, read_ordered, READ_SQL};
use grapt_store::EventStore;
use grapt_types::{ExecutionId, SessionId};
use rusqlite::{params, Connection, StatementStatus};
use serde_json::json;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Rows = Vec<(Option<String>, i64, i64, String, Option<String>)>;

// The accepted9b query is the comparison baseline, independent of mutants.
const ORIGINAL_READ_SQL: &str =
    "SELECT CASE WHEN length(CAST(id AS BLOB)) <= 256 THEN id END, seq, created,
         type, CASE WHEN length(CAST(data AS BLOB)) <= 4096 THEN data END
         FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
         AND type IN ('session.execution.started.1', 'session.execution.succeeded.1',
          'session.execution.failed.1', 'session.execution.interrupted.1')
         AND json_extract(data, '$.executionID') = ?2 ORDER BY seq LIMIT 3";

fn fixture() -> Result<Connection, Box<dyn std::error::Error>> {
    let store = EventStore::open_in_memory()?;
    let conn = Connection::open_in_memory()?;
    conn.execute_batch("PRAGMA foreign_keys = ON")?;
    for name in [
        "event_sequence",
        "event",
        "event_aggregate_seq_idx",
        "event_aggregate_type_seq_idx",
        "event_type_id_idx",
    ] {
        let sql: String = store.connection().query_row(
            "SELECT sql FROM sqlite_master WHERE name = ?1 AND type IN ('table', 'index')",
            [name],
            |row| row.get(0),
        )?;
        conn.execute_batch(&sql)?;
    }
    Ok(conn)
}

fn row(
    conn: &Connection,
    family: &str,
    aggregate: &str,
    seq: i64,
    kind: &str,
    data: &str,
) -> TestResult {
    conn.execute("INSERT OR IGNORE INTO event_sequence (aggregate_family, aggregate_id, seq) VALUES (?1, ?2, 0)", params![family, aggregate])?;
    conn.execute("INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch, envelope_bytes, campaign_transition_id)
         VALUES (?1, ?2, ?3, ?4, 7, ?5, CASE WHEN ?2 = 'campaign' THEN NULL ELSE ?6 END, 1,
         CASE WHEN ?2 = 'campaign' THEN X'00' END, CASE WHEN ?2 = 'campaign' THEN ?1 END)",
         params![format!("evt_br_exec_{family}_{aggregate}_{seq}"), family, aggregate, seq, kind, data])?;
    Ok(())
}

fn data(execution: &str) -> String {
    json!({"sessionID":"ses_br_execution", "executionID":execution}).to_string()
}

fn query(conn: &Connection, sql: &str, execution: &str) -> Result<(Rows, i32), rusqlite::Error> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement
        .query_map(params!["ses_br_execution", execution], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<Result<Rows, _>>()?;
    Ok((rows, statement.get_status(StatementStatus::VmStep)))
}

fn compared(conn: &Connection, execution: &str) -> Result<(i32, i32), Box<dyn std::error::Error>> {
    let original = ORIGINAL_READ_SQL;
    assert_ne!(original, READ_SQL, "execution ordering correction missing");
    let session: SessionId = "ses_br_execution".parse()?;
    let id = ExecutionId::new(execution).ok_or("fixture execution invalid")?;
    assert_eq!(
        read(conn, &session, &id),
        read_ordered(conn, &session, &id, original, false),
        "public decoder/refusal changed"
    );
    let old = query(conn, original, execution);
    let new = query(conn, READ_SQL, execution);
    match (old, new) {
        (Ok((a, baseline)), Ok((b, candidate))) => {
            assert_eq!(a, b, "execution selected rows differ");
            Ok((baseline, candidate))
        }
        (Err(a), Err(b)) => {
            assert_eq!(a.to_string(), b.to_string(), "query refusal differs");
            Ok((0, 0))
        }
        (Ok((rows, _)), Err(rusqlite::Error::SqliteFailure(_, Some(detail))))
            if !rows.is_empty() && detail == "malformed JSON" =>
        {
            Ok((0, 0))
        }
        (a, b) => panic!("execution query acceptance differs: {a:?} / {b:?}"),
    }
}

#[test]
fn br_execution_work_separates_content_from_other_executions() -> TestResult {
    for execution_noise in [false, true] {
        let conn = fixture()?;
        row(
            &conn,
            "session",
            "ses_br_execution",
            0,
            "session.execution.started.1",
            &data("exe_earliest"),
        )?;
        row(
            &conn,
            "session",
            "ses_br_execution",
            1,
            "session.execution.succeeded.1",
            &data("exe_earliest"),
        )?;
        let mut costs = Vec::new();
        for (start, count) in [(2, 64), (66, 1984)] {
            for seq in start..start + count {
                let kind = if execution_noise {
                    "session.execution.started.1"
                } else {
                    "session.message.created.1"
                };
                let payload = if execution_noise {
                    data(&format!("exe_noise_{seq}"))
                } else {
                    "not-json".to_owned()
                };
                row(&conn, "session", "ses_br_execution", seq, kind, &payload)?;
            }
            let latest = format!("exe_latest_{start}");
            row(
                &conn,
                "session",
                "ses_br_execution",
                start + count,
                "session.execution.started.1",
                &data(&latest),
            )?;
            costs.push([
                compared(&conn, "exe_absent")?,
                compared(&conn, "exe_earliest")?,
                compared(&conn, &latest)?,
            ]);
            // Remove only the temporary latest row so the next growth range is disjoint.
            conn.execute("DELETE FROM event WHERE aggregate_family = 'session' AND aggregate_id = 'ses_br_execution' AND seq = ?1", [start + count])?;
        }
        eprintln!("BR_EXEC_WORK execution_noise={execution_noise} costs={costs:?}");
        for (small, large) in costs[0].iter().zip(&costs[1]) {
            if execution_noise {
                assert!(
                    large.1 > small.1 * 8,
                    "JSON identity scans did not grow with owned kinds"
                );
            } else {
                assert!(
                    large.1 <= small.1 + 32,
                    "candidate scanned unrelated content"
                );
                assert!(
                    large.0 > small.0 * 8,
                    "baseline did not reproduce content scan"
                );
            }
        }
    }
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn br_execution_rows_and_refusals_match_before_selection() -> TestResult {
    let session: SessionId = "ses_br_execution".parse()?;
    let execution = ExecutionId::new("exe_target").ok_or("fixture execution id invalid")?;
    for case in [
        "absent",
        "start",
        "success",
        "failure",
        "future",
        "interrupt",
        "terminal-first",
        "duplicate-start",
        "third",
        "foreign-session",
        "malformed",
        "oversized",
        "empty-failure",
        "missing-failure",
        "error-on-success",
        "bad-interrupt",
        "extra-field",
    ] {
        let conn = fixture()?;
        row(
            &conn,
            "session",
            "ses_other",
            0,
            "session.execution.started.1",
            &data("exe_target"),
        )?;
        row(
            &conn,
            "campaign",
            "ses_br_execution",
            0,
            "session.execution.started.1",
            "ignored",
        )?;
        row(
            &conn,
            "session",
            "ses_br_execution",
            0,
            "session.message.created.1",
            "not-json",
        )?;
        if case != "absent" && case != "terminal-first" && case != "malformed" {
            row(
                &conn,
                "session",
                "ses_br_execution",
                1,
                "session.execution.started.1",
                &data("exe_target"),
            )?;
        }
        let (kind, payload) = match case {
            "absent" | "start" => ("session.message.created.1", "not-json".to_owned()),
            "failure" | "future" => ("session.execution.failed.1", json!({"sessionID":session.as_str(), "executionID":execution.as_str(), "error":{"type":"FutureFailure", "message":"future vocabulary survives"}}).to_string()),
            "empty-failure" => ("session.execution.failed.1", json!({"sessionID":session.as_str(), "executionID":execution.as_str(), "error":{"type":"FutureFailure", "message":""}}).to_string()),
            "interrupt" | "bad-interrupt" => ("session.execution.interrupted.1", json!({"sessionID":session.as_str(), "executionID":execution.as_str(), "reason": if case == "interrupt" {"shutdown"} else {"invented"}}).to_string()),
            "missing-failure" => ("session.execution.failed.1", data("exe_target")),
            "error-on-success" => ("session.execution.succeeded.1", json!({"sessionID":session.as_str(), "executionID":execution.as_str(), "error":{"type":"wrong", "message":"present"}}).to_string()),
            "duplicate-start" => ("session.execution.started.1", data("exe_target")),
            "foreign-session" => ("session.execution.succeeded.1", json!({"sessionID":"ses_wrong", "executionID":execution.as_str()}).to_string()),
            "malformed" => ("session.execution.started.1", "not-json".to_owned()),
            "oversized" => ("session.execution.failed.1", json!({"sessionID":session.as_str(), "executionID":execution.as_str(), "error":{"type":"large", "message":"x".repeat(5000)}}).to_string()),
            "extra-field" => ("session.execution.succeeded.1", json!({"sessionID":session.as_str(), "executionID":execution.as_str(), "unexpected":1}).to_string()),
            _ => ("session.execution.succeeded.1", data("exe_target")),
        };
        row(&conn, "session", "ses_br_execution", 2, kind, &payload)?;
        if case == "third" {
            row(
                &conn,
                "session",
                "ses_br_execution",
                3,
                "session.execution.failed.1",
                &data("exe_target"),
            )?;
        }
        compared(&conn, execution.as_str())?;
        let observed = read(&conn, &session, &execution);
        if [
            "absent",
            "start",
            "success",
            "failure",
            "future",
            "interrupt",
        ]
        .contains(&case)
        {
            let expected = match case {
                "absent" => 0,
                "start" => 1,
                _ => 2,
            };
            assert_eq!(observed?.len(), expected, "{case}");
        } else {
            assert!(observed.is_err(), "{case} corrupt history accepted");
        }
        eprintln!("BR_EXEC_CASE {case} matched");
    }
    let missing = Connection::open_in_memory()?;
    compared(&missing, execution.as_str())?;
    assert!(read(&missing, &session, &execution).is_err());
    Ok(())
}

#[test]
fn br_execution_third_observation_precedes_later_corruption() -> TestResult {
    let conn = fixture()?;
    for (seq, kind) in [
        (0, "session.execution.started.1"),
        (1, "session.execution.succeeded.1"),
        (2, "session.execution.failed.1"),
    ] {
        row(
            &conn,
            "session",
            "ses_br_execution",
            seq,
            kind,
            &if kind == "session.execution.failed.1" {
                json!({"sessionID":"ses_br_execution", "executionID":"exe_target", "error":{"type":"FutureFailure", "message":"duplicate terminal"}}).to_string()
            } else {
                data("exe_target")
            },
        )?;
    }
    row(
        &conn,
        "session",
        "ses_br_execution",
        3,
        "session.execution.started.1",
        "not-json",
    )?;
    compared(&conn, "exe_target")?;
    let session: SessionId = "ses_br_execution".parse()?;
    let execution = ExecutionId::new("exe_target").ok_or("fixture execution invalid")?;
    assert_eq!(
        read(&conn, &session, &execution),
        Err("execution has more than one terminal observation".to_owned())
    );
    Ok(())
}

#[test]
fn br_execution_json_recovery_preserves_the_first_decoder_refusal() -> TestResult {
    let conn = fixture()?;
    row(
        &conn,
        "session",
        "ses_br_execution",
        0,
        "session.execution.started.1",
        &json!({"sessionID":"ses_wrong", "executionID":"exe_target"}).to_string(),
    )?;
    row(
        &conn,
        "session",
        "ses_br_execution",
        1,
        "session.execution.failed.1",
        "not-json",
    )?;
    compared(&conn, "exe_target")?;
    let session: SessionId = "ses_br_execution".parse()?;
    let execution = ExecutionId::new("exe_target").ok_or("fixture execution invalid")?;
    assert_eq!(
        read(&conn, &session, &execution),
        Err("execution observation identity, order, or payload is invalid".to_owned())
    );
    Ok(())
}

#[test]
fn br_execution_isolates_families_sessions_and_payload_bounds() -> TestResult {
    let session: SessionId = "ses_br_execution".parse()?;
    let execution = ExecutionId::new("exe_target").ok_or("fixture execution invalid")?;
    let conn = fixture()?;
    row(
        &conn,
        "project",
        session.as_str(),
        0,
        "session.execution.started.1",
        &data(execution.as_str()),
    )?;
    row(
        &conn,
        "session",
        "ses_other",
        0,
        "session.execution.started.1",
        &data(execution.as_str()),
    )?;
    compared(&conn, execution.as_str())?;
    assert!(read(&conn, &session, &execution)?.is_empty());
    row(
        &conn,
        "session",
        session.as_str(),
        0,
        "session.execution.started.1",
        &data(execution.as_str()),
    )?;
    conn.execute(
        "UPDATE event SET id = ?1 WHERE aggregate_family = 'session' AND aggregate_id = ?2",
        params![format!("evt_{}", "x".repeat(256)), session.as_str()],
    )?;
    compared(&conn, execution.as_str())?;
    assert!(
        read(&conn, &session, &execution).is_err(),
        "oversized event identity accepted"
    );
    conn.execute("UPDATE event SET id = 'evt_bounded', created = -1 WHERE aggregate_family = 'session' AND aggregate_id = ?1", [session.as_str()])?;
    compared(&conn, execution.as_str())?;
    assert!(
        read(&conn, &session, &execution).is_err(),
        "negative timestamp accepted"
    );
    Ok(())
}
