//! Authenticated snapshots read committed truth and the live producer's prefix.
use super::*;

#[path = "../../../../test-support/tmp_guard.rs"]
mod tmp_guard;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn snapshot(address: SocketAddr, session: &str) -> TestResult<serde_json::Value> {
    let (status, body) = request(
        address,
        "GET",
        &format!("/api/session/{session}/snapshot"),
        "",
    );
    assert_eq!(status, 200, "snapshot: {}", String::from_utf8_lossy(&body));
    let _: grapt_api::u3a::SessionSnapshotResponseDto = serde_json::from_slice(&body)?;
    Ok(serde_json::from_slice(&body)?)
}

#[test]
fn snapshot_finishes_on_committed_sections_while_the_permission_writer_holds_commit() -> TestResult
{
    let _journey = process_journey_custody();
    let root = tmp_guard::Dir::exact_path(root("snapshot-held-permission"))?;
    prepare(&root, 60000);
    attended(&root);
    let barrier = root.join("snapshot-barrier");
    fs::create_dir(&barrier)?;
    let provider = ProviderFixture::start(Vec::new());
    let mut child = spawn_with(
        &root,
        provider.address,
        &[(
            "GRAPT_PERMISSION_VIEW_BARRIER",
            barrier.to_str().ok_or("barrier encoding")?,
        )],
    );
    let address = await_ready(&root, &mut child);
    let session = seed(address);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/form"),
        r#"{"id":"frm_snapshot","title":"Branch?","fields":[{"key":"branch","type":"string","required":true}]}"#,
    );
    let before = snapshot(address, &session)?;
    assert_eq!(before["permissions"].as_array().map(Vec::len), Some(1));
    assert_eq!(before["forms"].as_array().map(Vec::len), Some(1));
    fs::write(barrier.join("before-commit.arm"), [])?;
    let writer = reply(address, &session);
    entered(&barrier, "before-commit");
    let answer = std::panic::catch_unwind(|| {
        request_with_read_timeout(
            address,
            "GET",
            &format!("/api/session/{session}/snapshot"),
            "",
            Duration::from_secs(1),
        )
    });
    // Release and join the real writer before asserting the read outcome.
    fs::write(barrier.join("release"), [])?;
    let mutation = writer
        .join()
        .map_err(|_| "permission writer panicked")?
        .map_err(|_| "permission mutation request failed")?;
    assert_eq!(mutation.0, 204);
    let (status, body) =
        answer.map_err(|_| "snapshot waited for an unrelated writer's COMMIT lock")?;
    assert_eq!(status, 200);
    let during: serde_json::Value = serde_json::from_slice(&body)?;
    let _: grapt_api::u3a::SessionSnapshotResponseDto = serde_json::from_slice(&body)?;
    assert_eq!(during["revision"], before["revision"]);
    for section in ["rows", "cursor", "permissions", "forms"] {
        assert_eq!(
            during[section], before[section],
            "{section} escaped its pinned revision"
        );
    }
    let after = snapshot(address, &session)?;
    assert!(after["revision"]["seq"].as_u64() > before["revision"]["seq"].as_u64());
    assert_eq!(after["permissions"].as_array().map(Vec::len), Some(0));
    assert_eq!(after["forms"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        request(
            address,
            "POST",
            &format!("/api/session/{session}/form/frm_snapshot/reply"),
            r#"{"answer":{"branch":"main"}}"#
        )
        .0,
        204
    );
    let settled = snapshot(address, &session)?;
    assert_eq!(settled["forms"].as_array().map(Vec::len), Some(0));
    assert!(settled["revision"]["seq"].as_u64() > after["revision"]["seq"].as_u64());
    child.stop_after_witness(&root);
    Ok(())
}

static HOLD: AtomicBool = AtomicBool::new(false);
static TICKS: AtomicUsize = AtomicUsize::new(0);
struct Release;
impl Drop for Release {
    fn drop(&mut self) {
        HOLD.store(false, Ordering::SeqCst);
    }
}

#[test]
fn snapshot_carries_the_unfinished_live_prefix_then_the_committed_answer_without_changing_legacy_shape(
) -> TestResult {
    let _journey = process_journey_custody();
    let root = tmp_guard::Dir::exact_path(root("snapshot-live-prefix"))?;
    prepare(&root, 60000);
    attended(&root);
    HOLD.store(true, Ordering::SeqCst);
    TICKS.store(0, Ordering::SeqCst);
    let provider = ProviderFixture::start(vec![Script::Paced {
        label: "snapshot-visible",
        burst: 3,
        deltas: 1,
        interval: Duration::ZERO,
        ticks: &TICKS,
        hold: Some(&HOLD),
        breaks: false,
    }]);
    let _release = Release;
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = json(address, "POST", "/api/session", "{}")["data"]["id"]
        .as_str()
        .ok_or("session")?
        .to_owned();
    assert_eq!(
        request(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"text":"show a live answer","resume":true}"#
        )
        .0,
        200
    );
    let prefix = PollDeadline::new()
        .poll(Duration::from_millis(10), || {
            if TICKS.load(Ordering::SeqCst) < 3 {
                return None;
            }
            let value = snapshot(address, &session).ok()?;
            value["provisional"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty())
                .then_some(value)
        })
        .ok_or("snapshot never exposed the held live prefix")?;
    let block = prefix["provisional"]
        .as_array()
        .and_then(|rows| rows.first())
        .ok_or("provisional block")?;
    let text = block["text"].as_str().ok_or("provisional text")?;
    assert!(text.contains("snapshot-visible-000000"));
    assert_eq!(block["bytes"].as_u64(), Some(text.len() as u64));
    assert!(block["assistantMessageID"]
        .as_str()
        .is_some_and(|id| id.starts_with("msg_")));
    let (status, body) = request(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    );
    assert_eq!(status, 200);
    let _: grapt_api::u3a::SessionMessagesResponseDto = serde_json::from_slice(&body)?;
    let legacy: serde_json::Value = serde_json::from_slice(&body)?;
    assert!(legacy.get("revision").is_none() && legacy.get("provisional").is_none());
    HOLD.store(false, Ordering::SeqCst);
    permission_residuals::wait_session_idle(address, &session);
    let completed = snapshot(address, &session)?;
    assert_eq!(completed["provisional"].as_array().map(Vec::len), Some(0));
    assert!(completed["rows"]
        .to_string()
        .contains("snapshot-visible-000000"));
    assert!(completed["revision"]["seq"].as_u64() >= prefix["revision"]["seq"].as_u64());
    child.stop_after_witness(&root);
    Ok(())
}

#[test]
fn snapshot_and_legacy_rows_keep_the_same_omission_and_encoded_content() -> TestResult {
    let _journey = process_journey_custody();
    let root = tmp_guard::Dir::exact_path(root("snapshot-owned-omission"))?;
    prepare(&root, 60000);
    attended(&root);
    let provider = ProviderFixture::start(vec![Script::SuccessText("answer before the omission")]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = json(address, "POST", "/api/session", "{}")["data"]["id"]
        .as_str()
        .ok_or("session")?
        .to_owned();
    assert_eq!(
        request(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"text":"keep this user input intact","resume":true}"#
        )
        .0,
        200
    );
    permission_residuals::wait_session_idle(address, &session);
    snapshot(address, &session)?;
    // Only this disposable fixture's owned durable item is corrupted.
    let connection = rusqlite::Connection::open(root.join("database.sqlite3"))?;
    let seq: i64 = connection.query_row(
        "SELECT seq FROM event WHERE aggregate_family='session' AND aggregate_id=?1
         AND type='session.content.appended.1' ORDER BY seq DESC LIMIT 1",
        [&session],
        |row| row.get(0),
    )?;
    assert_eq!(connection.execute(
        "UPDATE event SET data='{' WHERE aggregate_family='session' AND aggregate_id=?1 AND seq=?2",
        rusqlite::params![session, seq])?, 1);
    drop(connection);
    let (status, legacy_bytes) = request(
        address,
        "GET",
        &format!("/api/session/{session}/message?limit=200&order=desc"),
        "",
    );
    assert_eq!(
        status,
        200,
        "legacy: {}",
        String::from_utf8_lossy(&legacy_bytes)
    );
    let legacy: grapt_api::u3a::SessionMessagesResponseDto = serde_json::from_slice(&legacy_bytes)?;
    let value: serde_json::Value = serde_json::from_slice(&legacy_bytes)?;
    assert!(
        value["data"]
            .as_array()
            .ok_or("legacy rows")?
            .iter()
            .any(|row| {
                row["metadata"]["omittedTotal"]
                    .as_u64()
                    .is_some_and(|total| total >= 1)
                    && row["metadata"]["omittedItems"]
                        .as_array()
                        .is_some_and(|gaps| gaps.iter().any(|gap| gap["seq"].as_i64() == Some(seq)))
            }),
        "the actual unreadable coordinate must be visible in the legacy rows"
    );
    let (status, snapshot_bytes) = request(
        address,
        "GET",
        &format!("/api/session/{session}/snapshot"),
        "",
    );
    assert_eq!(
        status,
        200,
        "snapshot: {}",
        String::from_utf8_lossy(&snapshot_bytes)
    );
    let current: grapt_api::u3a::SessionSnapshotResponseDto =
        serde_json::from_slice(&snapshot_bytes)?;
    assert_eq!(
        serde_json::to_vec(current.rows().get())?,
        serde_json::to_vec(legacy.data().get())?
    );
    assert_eq!(
        serde_json::to_vec(current.cursor().get())?,
        serde_json::to_vec(legacy.cursor().get())?
    );
    child.stop_after_witness(&root);
    Ok(())
}
