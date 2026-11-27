//! Real authenticated GETs through the server while its writer holds COMMIT.
use super::*;

#[path = "session_snapshot_tests.rs"]
mod snapshot;

type MutationReply = Result<(u16, Vec<u8>), Box<dyn std::any::Any + Send>>;

fn entered(root: &Path, phase: &str) {
    let wait = PollDeadline::new();
    while wait.pending() {
        if root.join(format!("{phase}.entered")).is_file() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("real permission mutation never entered {phase} barrier ({wait})");
}

fn seed(address: SocketAddr) -> String {
    let session = json(address, "POST", "/api/session", "{}");
    let id = session["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("permission view fixture Session omitted ID"))
        .to_owned();
    let asked = json(
        address,
        "POST",
        &format!("/api/session/{id}/permission"),
        r#"{"id":"per_committed_view","agent":"agt_live_headless1","action":"write","resources":["/work/view"]}"#,
    );
    assert_eq!(asked["data"]["effect"], "ask");
    id
}

fn reply(address: SocketAddr, session: &str) -> thread::JoinHandle<MutationReply> {
    let path = format!("/api/session/{session}/permission/per_committed_view/reply");
    thread::spawn(move || {
        std::panic::catch_unwind(|| request(address, "POST", &path, r#"{"reply":"once"}"#))
    })
}

#[test]
fn authenticated_permission_poll_finishes_before_real_commit_barrier_releases() {
    let _journey = process_journey_custody();
    let root = root("permission-view-held-commit");
    prepare(&root, 60000);
    attended(&root);
    let barrier = root.join("view-barrier");
    fs::create_dir(&barrier).unwrap_or_else(|error| panic!("{error}"));
    let provider = ProviderFixture::start(Vec::new());
    let mut child = spawn_with(
        &root,
        provider.address,
        &[(
            "GRAPT_PERMISSION_VIEW_BARRIER",
            barrier
                .to_str()
                .unwrap_or_else(|| panic!("barrier root encoding")),
        )],
    );
    let address = await_ready(&root, &mut child);
    let session = seed(address);
    fs::write(barrier.join("before-commit.arm"), []).unwrap_or_else(|error| panic!("{error}"));
    let writer = reply(address, &session);
    entered(&barrier, "before-commit");
    let health = std::panic::catch_unwind(|| {
        request_with_read_timeout(address, "GET", "/api/health", "", Duration::from_secs(1))
    });
    let poll = std::panic::catch_unwind(|| {
        request_with_read_timeout(
            address,
            "GET",
            &format!("/api/session/{session}/permission"),
            "",
            Duration::from_secs(1),
        )
    });
    // Always release and join the writer before checking the poll's outcome.
    fs::write(barrier.join("release"), []).unwrap_or_else(|error| panic!("{error}"));
    let mutation = writer
        .join()
        .unwrap_or_else(|_| panic!("permission mutation thread panicked"))
        .unwrap_or_else(|_| panic!("permission mutation request failed"));
    assert_eq!(mutation.0, 204);
    assert_eq!(
        health
            .unwrap_or_else(|_| panic!("health GET waited for staged COMMIT"))
            .0,
        200
    );
    let (status, body) =
        poll.unwrap_or_else(|_| panic!("authenticated permission GET waited for staged COMMIT"));
    assert_eq!(status, 200);
    let page: serde_json::Value =
        serde_json::from_slice(&body).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        page["data"][0]["id"], "per_committed_view",
        "uncommitted reply was published"
    );
    let page = json(
        address,
        "GET",
        &format!("/api/session/{session}/permission"),
        "",
    );
    assert_eq!(
        page["data"].as_array().map(Vec::len),
        Some(0),
        "mutation ACK preceded publication"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    child.stop_after_witness(&root);
    process_fixture::cleanup(&root);
}

#[test]
fn crash_after_commit_before_publication_rebuilds_without_resurrecting_permission() {
    let _journey = process_journey_custody();
    let root = root("permission-view-crash");
    prepare(&root, 60000);
    attended(&root);
    let barrier = root.join("view-barrier");
    fs::create_dir(&barrier).unwrap_or_else(|error| panic!("{error}"));
    let provider = ProviderFixture::start(Vec::new());
    let mut child = spawn_with(
        &root,
        provider.address,
        &[(
            "GRAPT_PERMISSION_VIEW_BARRIER",
            barrier
                .to_str()
                .unwrap_or_else(|| panic!("barrier root encoding")),
        )],
    );
    let address = await_ready(&root, &mut child);
    let session = seed(address);
    fs::write(barrier.join("after-commit.arm"), []).unwrap_or_else(|error| panic!("{error}"));
    let writer = reply(address, &session);
    entered(&barrier, "after-commit");
    // COMMIT returned successfully, publication has not executed, and no ACK
    // was sent. Kill this exact process and preserve its database for restart.
    child.stop_after_witness(&root);
    let _transport_loss = writer
        .join()
        .unwrap_or_else(|_| panic!("permission mutation thread panicked"));
    let connection = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap_or_else(|error| panic!("{error}"));
    let status: String = connection
        .query_row(
            "SELECT status FROM permission_request WHERE id = 'per_committed_view'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("durable reply missing after crash: {error}"));
    assert_eq!(status, "replied");
    drop(connection);
    let mut restarted = spawn(&root, provider.address);
    let address = await_ready(&root, &mut restarted);
    let page = json(
        address,
        "GET",
        &format!("/api/session/{session}/permission"),
        "",
    );
    assert_eq!(
        page["data"].as_array().map(Vec::len),
        Some(0),
        "restart resurrected terminal permission"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    restarted.stop_after_witness(&root);
    process_fixture::cleanup(&root);
}
