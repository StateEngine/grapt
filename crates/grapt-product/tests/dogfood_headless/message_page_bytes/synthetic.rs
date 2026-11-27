//! Synthetic inputs remain readable through both transcript consumers.
use super::*;
use grapt_inbox::{Inbox, SyntheticAdmitRequest};
use grapt_store::AggregateId;
use grapt_types::{DeliveryMode, EventId, MessageId};

type TestResult = Result<(), Box<dyn std::error::Error>>;
const INPUT: &str = "msg_transcript_synthetic";
const TEXT: &str = "Continue the admitted synthetic task";
const WAKE: &str = "Resume the separate wake input";

fn prompt(address: SocketAddr, session: &str, id: &str, text: &str) {
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":id,"text":text,"delivery":"queue","resume":true}).to_string(),
    );
}

fn admit(root: &Path, session: &str, metadata: Option<&serde_json::Value>) -> TestResult {
    let session: SessionId = session.parse()?;
    let mut store = EventStore::open(&root.join("database.sqlite3"), "live-headless1")?;
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    Inbox::new(store)?.admit_synthetic(SyntheticAdmitRequest {
        event_id: &"evt_transcript_synthetic".parse::<EventId>()?,
        session: &session,
        input: &INPUT.parse::<MessageId>()?,
        text: TEXT,
        description: None,
        metadata,
        delivery: DeliveryMode::Queue,
        resume: true,
        created_ms: 1_790_000_000_000,
    })?;
    Ok(())
}

fn assert_row(row: &serde_json::Value, metadata: Option<&serde_json::Value>, seq: i64) {
    assert_eq!(row["id"], INPUT);
    if metadata.is_some_and(serde_json::Value::is_null) {
        assert_eq!(row["type"], "system");
        assert_eq!(row["metadata"]["unreadableMessage"], true);
        assert_eq!(row["metadata"]["seq"], seq);
        assert!(row["text"]
            .as_str()
            .is_some_and(|text| text.contains("could not be read")));
    } else {
        assert_eq!(row["type"], "synthetic");
        assert_eq!(row["text"], TEXT);
        assert_eq!(row.get("metadata"), metadata);
        assert!(row.get("description").is_none());
    }
}

fn read<T: serde::de::DeserializeOwned>(
    address: SocketAddr,
    path: &str,
) -> Result<(T, serde_json::Value), Box<dyn std::error::Error>> {
    let (status, bytes) = request(address, "GET", path, "");
    assert_eq!(status, 200, "{path}: {}", String::from_utf8_lossy(&bytes));
    Ok((
        serde_json::from_slice(&bytes)?,
        serde_json::from_slice(&bytes)?,
    ))
}

fn views(
    address: SocketAddr,
    session: &str,
    metadata: Option<&serde_json::Value>,
    seq: i64,
) -> TestResult {
    let (_, page) = read::<grapt_api::u3a::SessionMessagesResponseDto>(
        address,
        &format!("/api/session/{session}/message?order=asc&limit=50"),
    )?;
    let (_, snapshot) = read::<grapt_api::u3a::SessionSnapshotResponseDto>(
        address,
        &format!("/api/session/{session}/snapshot"),
    )?;
    for rows in [&page["data"], &snapshot["rows"]] {
        let rows = rows.as_array().ok_or("transcript rows")?;
        let row = rows
            .iter()
            .find(|row| row["id"] == INPUT)
            .ok_or("synthetic row")?;
        assert_row(row, metadata, seq);
        for neighbour in ["msg_transcript_original", "msg_transcript_wake"] {
            assert!(
                rows.iter().any(|row| row["id"] == neighbour),
                "lost neighbour {neighbour}"
            );
        }
    }
    let (_, single) =
        read::<serde_json::Value>(address, &format!("/api/session/{session}/message/{INPUT}"))?;
    assert_row(&single["data"], metadata, seq);
    Ok(())
}

fn check(metadata: Option<serde_json::Value>, label: &str) -> TestResult {
    let _journey = process_journey_custody();
    let root = root(label);
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(vec![Script::Success, Script::Success, Script::Success]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    prompt(
        address,
        &session,
        "msg_transcript_original",
        "Complete the original task",
    );
    manual_compaction::assert_carried(
        &root,
        "msg_transcript_original",
        "session.execution.succeeded.1",
    );
    permission_residuals::wait_session_idle(address, &session);
    child.stop_after_witness(&root);
    drop(child);
    admit(&root, &session, metadata.as_ref())?;
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    prompt(address, &session, "msg_transcript_wake", WAKE);
    manual_compaction::assert_carried(&root, INPUT, "session.execution.succeeded.1");
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    {
        let bodies = locked(&provider.bodies);
        assert!(bodies[1].contains(TEXT));
        assert!(bodies[2].contains(WAKE));
    }
    let db = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let (seq, stored_metadata, description): (i64, Option<String>, Option<String>) = db.query_row(
        "SELECT seq,metadata_json,description FROM session_message WHERE id=?1",
        [INPUT],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if metadata.is_none() {
        assert!(stored_metadata.is_none() && description.is_none());
    }
    if metadata.as_ref().is_some_and(serde_json::Value::is_null) {
        assert_eq!(stored_metadata.as_deref(), Some("null"));
    }
    drop(db);
    views(address, &session, metadata.as_ref(), seq)?;
    child.stop_after_witness(&root);
    Ok(())
}

#[test]
fn review_absent_synthetic_fields_remain_absent_after_restart() -> TestResult {
    check(None, "transcript-absent")
}

#[test]
fn review_object_synthetic_metadata_survives_every_transcript_view() -> TestResult {
    check(
        Some(serde_json::json!({"source":"fixture"})),
        "transcript-object",
    )
}

#[test]
fn review_stored_null_keeps_its_coordinate_and_neighbours() -> TestResult {
    check(Some(serde_json::Value::Null), "transcript-null")
}
