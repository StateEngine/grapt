//! BR: query work and durable-read semantics on the bundled SQLite engine.
use super::{SessionStore, SessionStoreError, DELETED_SQL, LIFECYCLE_SQL};
use grapt_store::EventStore;
use grapt_types::SessionId;
use rusqlite::{params, Connection, Params, StatementStatus};
use serde_json::{json, Value};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type RawRows = Vec<(String, Option<String>)>;

// Stage only disposable, in-memory rows through a raw connection. Copy the
// actual store table/index definitions, including its family-shape constraint;
// a simplified schema could make an impossible collision pass this witness.
fn fixture() -> Result<Connection, Box<dyn std::error::Error>> {
    let store = EventStore::open_in_memory()?;
    let conn = Connection::open_in_memory()?;
    conn.execute_batch("PRAGMA foreign_keys = ON")?;
    for name in [
        "event_sequence",
        "event",
        "event_aggregate_seq_idx",
        "event_aggregate_type_seq_idx",
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

fn session() -> Result<SessionId, grapt_types::IdError> {
    "ses_read_work".parse()
}

fn seed(session: &SessionId, title: &str) -> Value {
    json!({"sessionID":session.as_str(),"info":{
        "id":session.as_str(),"projectID":"global","directory":"/work/read",
        "title":title,"slug":"read","version":"1","time":{"created":7,"updated":7}}})
}

fn row(conn: &Connection, family: &str, seq: i64, kind: &str, data: &str) -> TestResult {
    let session = session()?;
    conn.execute(
        "INSERT OR IGNORE INTO event_sequence (aggregate_family, aggregate_id, seq)
         VALUES (?1, ?2, 0)",
        params![family, session.as_str()],
    )?;
    conn.execute(
        "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type,
                            data, writer_epoch, envelope_bytes, campaign_transition_id)
         VALUES (?1, ?2, ?3, ?4, 7, ?5,
                 CASE WHEN ?2 = 'campaign' THEN NULL ELSE ?6 END, 1,
                 CASE WHEN ?2 = 'campaign' THEN X'00' END,
                 CASE WHEN ?2 = 'campaign' THEN ?1 END)",
        params![
            format!("evt_read_{family}_{seq}"),
            family,
            session.as_str(),
            seq,
            kind,
            data
        ],
    )?;
    Ok(())
}

fn noise(conn: &Connection, start: i64, count: i64) -> TestResult {
    for seq in start..start + count {
        // An unrelated corrupt payload must never enter a lifecycle decode.
        row(
            conn,
            "session",
            seq,
            "session.execution.started.1",
            "not-json",
        )?;
    }
    Ok(())
}

fn measured(
    conn: &Connection,
    sql: &str,
    parameters: impl Params,
) -> Result<(RawRows, i32), rusqlite::Error> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement
        .query_map(parameters, |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<RawRows, _>>()?;
    Ok((rows, statement.get_status(StatementStatus::VmStep)))
}

fn compared(
    conn: &Connection,
    sql: &str,
    parameters: &[&dyn rusqlite::ToSql],
) -> Result<(i32, i32), rusqlite::Error> {
    let (selected, steps) = measured(conn, sql, parameters)?;
    let old = sql.replace("ORDER BY +seq", "ORDER BY seq");
    let (baseline, baseline_steps) = measured(conn, &old, parameters)?;
    assert_eq!(selected, baseline, "query ordering changed selected rows");
    assert!(
        steps > 0 && steps < 512,
        "BR query scanned unrelated history: {steps} VM steps"
    );
    Ok((steps, baseline_steps))
}

#[test]
fn br_queries_select_owned_kinds_without_visiting_unrelated_history() -> TestResult {
    let conn = fixture()?;
    let session = session()?;
    row(
        &conn,
        "session",
        0,
        "session.created.1",
        &seed(&session, "first").to_string(),
    )?;
    let mut costs = Vec::new();
    for (start, count) in [(1, 64), (65, 1_984)] {
        noise(&conn, start, count)?;
        let current = vec![
            compared(&conn, LIFECYCLE_SQL, params!["session", session.as_str()])?,
            compared(&conn, DELETED_SQL, params![session.as_str(), 256])?,
        ];
        costs.push(current);
    }
    assert_eq!(costs[0].len(), 2);
    assert_eq!(costs[1].len(), 2);
    for (initial, later) in costs[0].iter().zip(&costs[1]) {
        assert!(
            later.0 <= initial.0 + 32,
            "owned query work grew with unrelated rows"
        );
        assert!(
            later.1 > initial.1 * 8,
            "baseline did not reproduce the history scan"
        );
    }
    eprintln!("BR_QUERY_WORK {costs:?}");
    Ok(())
}

#[test]
fn br_interleaved_lifecycle_rows_preserve_order_and_first_seed() -> TestResult {
    let conn = fixture()?;
    let session = session()?;
    row(
        &conn,
        "session",
        0,
        "session.created.1",
        &seed(&session, "first").to_string(),
    )?;
    noise(&conn, 1, 16)?;
    let mut duplicate = seed(&session, "duplicate");
    duplicate["info"]["directory"] = json!("/work/duplicate");
    duplicate["info"]["time"]["created"] = json!(999);
    for (seq, kind, payload) in [
        (
            17,
            "session.renamed.1",
            json!({"sessionID":session.as_str(),"title":"renamed"}),
        ),
        (
            18,
            "session.preset.selected.1",
            json!({"sessionID":session.as_str(),"preset":"p"}),
        ),
        (19, "session.created.1", duplicate),
        (
            20,
            "session.agent.selected.1",
            json!({"sessionID":session.as_str(),"agent":"build"}),
        ),
        (
            21,
            "session.model.selected.1",
            json!({"sessionID":session.as_str(),"model":{"id":"m","providerID":"p"}}),
        ),
        (
            22,
            "session.preset.selected.1",
            json!({"sessionID":session.as_str(),"preset":null}),
        ),
        (
            23,
            "session.renamed.1",
            json!({"sessionID":session.as_str(),"title":"last"}),
        ),
    ] {
        row(&conn, "session", seq, kind, &payload.to_string())?;
    }
    let view = SessionStore::new(&conn);
    let kinds = view
        .lifecycle_rows(&session)?
        .into_iter()
        .map(|(kind, _, _)| kind)
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            "session.created",
            "session.renamed",
            "session.preset.selected",
            "session.created",
            "session.agent.selected",
            "session.model.selected",
            "session.preset.selected",
            "session.renamed"
        ]
    );
    let record = view.record(&session)?;
    assert_eq!(record.title, "last");
    assert_eq!(record.agent.as_deref(), Some("build"));
    assert_eq!(record.model, Some(json!({"id":"m","providerID":"p"})));
    assert_eq!(record.preset, None);
    assert_eq!(record.created_ms, 7);
    assert_eq!(record.placement.directory(), "/work/read");
    Ok(())
}

#[test]
fn br_foreign_family_cannot_delete_or_supply_lifecycle_rows() -> TestResult {
    let conn = fixture()?;
    let session = session()?;
    row(
        &conn,
        "session",
        0,
        "session.created.1",
        &seed(&session, "real").to_string(),
    )?;
    row(&conn, "campaign", 0, "session.created.1", "")?;
    row(&conn, "campaign", 1, "session.deleted.2", "")?;
    let view = SessionStore::new(&conn);
    assert!(
        view.exists(&session)?,
        "foreign deletion hid a live session"
    );
    assert_eq!(
        view.record(&session)?.title,
        "real",
        "foreign lifecycle changed the record"
    );
    assert_eq!(view.lifecycle_rows(&session)?.len(), 1);
    Ok(())
}

#[test]
fn br_earliest_deletion_keeps_versions_and_corruption_refusal() -> TestResult {
    let session = session()?;
    let limit = grapt_event::LegacyDataLimit::MEASURED_DEFAULT.max_data_bytes();
    let oversized = "x".repeat(limit.checked_add(1).ok_or("payload bound overflow")?);
    for (first, payload, second, corrupt) in [
        (
            "session.deleted.1",
            seed(&session, "deleted").to_string(),
            "session.deleted.2",
            false,
        ),
        ("session.deleted.2", "{}".into(), "session.deleted.1", false),
        (
            "session.deleted.1",
            "not-json".into(),
            "session.deleted.2",
            true,
        ),
        ("session.deleted.1", oversized, "session.deleted.2", true),
    ] {
        let conn = fixture()?;
        row(
            &conn,
            "session",
            0,
            "session.created.1",
            &seed(&session, "live").to_string(),
        )?;
        noise(&conn, 1, 16)?;
        row(&conn, "session", 17, first, &payload)?;
        row(&conn, "session", 18, second, "not-json")?;
        let view = SessionStore::new(&conn);
        let result = view.record(&session);
        if corrupt {
            assert!(
                matches!(result, Err(SessionStoreError::Corrupt { .. })),
                "malformed earliest deletion was accepted: {result:?}"
            );
            assert!(matches!(
                view.exists(&session),
                Err(SessionStoreError::Corrupt { .. })
            ));
        } else {
            assert!(
                matches!(result, Err(SessionStoreError::Unknown { .. })),
                "deleted session resurrected: {result:?}"
            );
            assert!(!view.exists(&session)?);
        }
    }
    Ok(())
}

#[test]
fn br_absence_corruption_and_failed_reads_remain_distinct() -> TestResult {
    let conn = fixture()?;
    let session = session()?;
    assert!(matches!(
        SessionStore::new(&conn).record(&session),
        Err(SessionStoreError::Unknown { .. })
    ));
    row(&conn, "session", 0, "session.created.1", "not-json")?;
    assert!(matches!(
        SessionStore::new(&conn).record(&session),
        Err(SessionStoreError::Corrupt { .. })
    ));
    conn.execute_batch("ALTER TABLE event RENAME TO missing_event")?;
    assert!(matches!(
        SessionStore::new(&conn).lifecycle_rows(&session),
        Err(SessionStoreError::Store { .. })
    ));
    assert!(matches!(
        SessionStore::new(&conn).record(&session),
        Err(SessionStoreError::Store { .. })
    ));
    Ok(())
}
