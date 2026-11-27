//! Deliberate overwrite through the product's permission path: replacing an
//! Author preset asks as fs.overwrite before replacing an existing file,
//! while creating one runs unasked.
use super::*;

/// The pending ask for `call`, once it appears.
pub(super) fn asked(address: SocketAddr, session: &str, call: &str) -> serde_json::Value {
    let wait = PollDeadline::new();
    wait.poll(Duration::from_millis(50), || {
        json(
            address,
            "GET",
            &format!("/api/session/{session}/permission"),
            "",
        )["data"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["source"]["callID"] == call))
            .cloned()
    })
    .unwrap_or_else(|| panic!("no permission was asked for {call}"))
}

pub(super) fn reply(address: SocketAddr, session: &str, ask: &serde_json::Value, reply: &str) {
    let id = ask["id"]
        .as_str()
        .unwrap_or_else(|| panic!("permission has no id: {ask}"));
    let body = serde_json::json!({"reply": reply}).to_string();
    assert_eq!(
        request(
            address,
            "POST",
            &format!("/api/session/{session}/permission/{id}/reply"),
            &body,
        )
        .0,
        204
    );
}

fn prompt(address: SocketAddr, session: &str, id: &str, text: &str) {
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id": id, "text": text, "delivery": "queue", "resume": true})
            .to_string(),
    );
}

#[test]
fn an_unasked_create_never_covers_an_overwrite() {
    let _journey = process_journey_custody();
    let root = root("write-overwrite-approvals");
    prepare(&root, 120_000);
    attended(&root);
    let provider = ProviderFixture::start(vec![
        write_call("call_write_create", "notes.txt", "v1\n"),
        Script::SuccessText("created"),
        write_call("call_write_replace", "notes.txt", "v2\n"),
        Script::SuccessText("replaced"),
        write_call("call_write_again", "notes.txt", "v3\n"),
        Script::SuccessText("replaced again"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);

    // Creating a file runs unasked under the author preset.
    prompt(address, &session, "msg_write_create", "create notes.txt");
    permission_residuals::wait_session_idle(address, &session);
    let notes = root.join("notes.txt");
    assert_eq!(fs::read(&notes).unwrap_or_default(), b"v1\n");
    let unasked = json(
        address,
        "GET",
        &format!("/api/session/{session}/permission"),
        "",
    );
    assert_eq!(
        unasked["data"],
        serde_json::json!([]),
        "creating a file asked"
    );

    // What runs unasked for a new file never covers replacing one.
    prompt(address, &session, "msg_write_replace", "replace notes.txt");
    let replace = asked(address, &session, "call_write_replace");
    assert_eq!(replace["action"], "fs.overwrite", "{replace}");
    reply(address, &session, &replace, "session");
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(fs::read(&notes).unwrap_or_default(), b"v2\n");

    // An fs.overwrite approval for the Session covers the next overwrite.
    prompt(address, &session, "msg_write_again", "replace it again");
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(fs::read(&notes).unwrap_or_default(), b"v3\n");

    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}
