//! A failed authoritative observation cannot leave a live view silently stale.
use super::*;

#[test]
fn an_uncommitted_permission_wait_reconciles_the_connected_stream() {
    let _journey = process_journey_custody();
    let root = root("permission-status-commit-refusal");
    prepare(&root, 90_000);
    attended(&root);
    let target = root.join("proposal.txt");
    fs::write(&target, b"original\n").unwrap_or_else(|error| panic!("fixture file: {error}"));
    let database = rusqlite::Connection::open(root.join("database.sqlite3"))
        .unwrap_or_else(|error| panic!("fixture database: {error}"));
    database
        .execute_batch(
            "CREATE TRIGGER refuse_permission_status BEFORE INSERT ON event
             WHEN NEW.type = 'session.status.changed.1'
               AND json_extract(NEW.data, '$.state.reason') = 'permission'
             BEGIN SELECT RAISE(FAIL, 'synthetic status append refusal'); END;",
        )
        .unwrap_or_else(|error| panic!("fixture status refusal: {error}"));
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_status_commit",
            tool: "write",
            arguments: serde_json::json!({"path":"proposal.txt","content":"replacement\n"}),
        },
        Script::Success,
    ]);
    let mut child = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let approver = approval::Attachment::open(address, &session);
    let (mut stream, _) = event_stream(address);
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap_or_else(|error| panic!("stream timeout: {error}"));
    let _accepted = json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_status_commit","text":"replace the proposed file","delivery":"queue","resume":true}"#,
    );
    let mut bytes = 0usize;
    let mut failure = None;
    let ended = permission_residuals::wait_until(
        client_allowance::CLI_REQUEST_ALLOWANCE,
        permission_residuals::SETTLE_POLL,
        || {
            let mut chunk = [0u8; 4096];
            match stream.read(&mut chunk) {
                Ok(0) => true,
                Ok(count) => {
                    bytes += count;
                    assert!(bytes <= 4 * 1024 * 1024, "stream exceeded its byte bound");
                    false
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    false
                }
                Err(error) => {
                    failure = Some(error);
                    true
                }
            }
        },
    );
    let info = json(address, "GET", &format!("/api/session/{session}"), "");
    let asks: i64 = database
        .query_row(
            "SELECT COUNT(*) FROM permission_request WHERE session_id = ?1",
            [&session],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("committed permission read: {error}"));
    drop(stream);
    drop(approver);
    drop(database);
    child.stop_after_witness(&root);
    drop(child);
    let preserved = fs::read(&target).unwrap_or_else(|error| panic!("fixture file: {error}"));
    process_fixture::cleanup(&root);
    assert!(asks > 0, "the fixture never committed a permission request");
    assert_eq!(preserved, b"original\n", "the unapproved write ran");
    assert!(
        failure.is_none(),
        "stream failed instead of reconciling: {failure:?}"
    );
    assert!(
        ended.is_some(),
        "the stream stayed open after a status append refusal: {info}"
    );
}
