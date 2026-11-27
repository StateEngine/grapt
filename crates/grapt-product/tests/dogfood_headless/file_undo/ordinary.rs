//! Ordinary mutations participate in the same prompt checkpoint and restore.
use super::*;

#[test]
fn edit_shares_undo_with_patch_creation_and_write_overwrite() {
    let _journey = process_journey_custody();
    let service = root("undo-ordinary-edit-service");
    prepare(&service, 120_000);
    attended(&service);
    let workspace = workspace("undo-ordinary-edit-workspace");
    write(&workspace.join("edit.txt"), "before\r\n");
    write(&workspace.join("overwrite.txt"), "old overwrite");
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_ordinary_patch",
            tool: "patch",
            arguments: serde_json::json!({"operations":[update("edit.txt", "before\r\n", "middle\r\n")]}),
        },
        Script::ToolInput {
            call: "call_ordinary_edit",
            tool: "edit",
            arguments: serde_json::json!({"path":"edit.txt","old_text":"middle\n","new_text":"after\n"}),
        },
        Script::ToolInput {
            call: "call_ordinary_create",
            tool: "write",
            arguments: serde_json::json!({"path":"created.txt","content":"created bytes"}),
        },
        Script::ToolInput {
            call: "call_ordinary_overwrite",
            tool: "write",
            arguments: serde_json::json!({"path":"overwrite.txt","content":"new overwrite"}),
        },
        Script::Success,
    ]);
    let mut child = spawn(&service, provider.address);
    let address = await_ready(&service, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create_in(address, &workspace);
    prompt(address, &session, "msg_ordinary_edit");
    compaction_continuity::approve_batch(
        address,
        &session,
        &[
            "call_ordinary_patch",
            "call_ordinary_edit",
            "call_ordinary_create",
            "call_ordinary_overwrite",
        ],
    );
    wait_calls(&provider.calls, 5);
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(read(&workspace.join("edit.txt")), "after\r\n");
    assert_eq!(read(&workspace.join("overwrite.txt")), "new overwrite");
    let selected = preview(address, &session);
    let mut paths = selected["data"]["paths"]
        .as_array()
        .unwrap_or_else(|| panic!("undo paths absent: {selected}"))
        .clone();
    paths.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
    assert_eq!(
        serde_json::Value::Array(paths),
        serde_json::json!(["created.txt", "edit.txt", "overwrite.txt"])
    );
    let body = undo_body(&selected);
    let route = format!("/api/session/{session}/undo");
    write(&workspace.join("edit.txt"), "later human edit");
    assert_eq!(request(address, "POST", &route, &body).0, 409);
    assert_eq!(read(&workspace.join("edit.txt")), "later human edit");
    assert_eq!(read(&workspace.join("overwrite.txt")), "new overwrite");
    assert!(workspace.join("created.txt").exists());
    write(&workspace.join("edit.txt"), "after\r\n");
    assert_eq!(
        json(address, "POST", &route, &body)["data"]["status"],
        "applied"
    );
    assert_eq!(read(&workspace.join("edit.txt")), "before\r\n");
    assert_eq!(read(&workspace.join("overwrite.txt")), "old overwrite");
    assert!(!workspace.join("created.txt").exists());
    child.stop_after_witness(&service);
    process_fixture::cleanup(&workspace);
    process_fixture::cleanup(&service);
}

#[test]
fn write_creation_and_binary_overwrite_undo_after_service_restart() {
    let _journey = process_journey_custody();
    let service = root("undo-ordinary-write-service");
    prepare(&service, 120_000);
    attended(&service);
    let workspace = workspace("undo-ordinary-write-workspace");
    let before = b"\0\xffbefore\r\n";
    fs::write(workspace.join("binary.dat"), before)
        .unwrap_or_else(|error| panic!("binary fixture write: {error}"));
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_write_create",
            tool: "write",
            arguments: serde_json::json!({"path":"created.txt","content":"created"}),
        },
        Script::ToolInput {
            call: "call_write_replace",
            tool: "write",
            arguments: serde_json::json!({"path":"binary.dat","content":"replacement"}),
        },
        Script::Success,
    ]);
    let mut child = spawn(&service, provider.address);
    let address = await_ready(&service, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create_in(address, &workspace);
    prompt(address, &session, "msg_ordinary_write");
    compaction_continuity::approve_batch(
        address,
        &session,
        &["call_write_create", "call_write_replace"],
    );
    wait_calls(&provider.calls, 3);
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(read(&workspace.join("created.txt")), "created");
    assert_eq!(read(&workspace.join("binary.dat")), "replacement");
    child.stop_after_witness(&service);
    drop(child);
    let mut child = spawn(&service, provider.address);
    let address = await_ready(&service, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let selected = preview(address, &session);
    assert_eq!(selected["data"]["paths"].as_array().map(Vec::len), Some(2));
    let receipt = json(
        address,
        "POST",
        &format!("/api/session/{session}/undo"),
        &undo_body(&selected),
    );
    assert_eq!(receipt["data"]["status"], "applied");
    assert!(!workspace.join("created.txt").exists());
    assert_eq!(
        fs::read(workspace.join("binary.dat"))
            .unwrap_or_else(|error| panic!("restored binary: {error}")),
        before
    );
    child.stop_after_witness(&service);
    process_fixture::cleanup(&workspace);
    process_fixture::cleanup(&service);
}
