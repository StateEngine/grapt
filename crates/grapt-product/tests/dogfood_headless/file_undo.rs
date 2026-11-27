//! PU: real patch capture, exact file restore, and journal recovery after loss.
use super::*;
use sha2::{Digest, Sha256};
#[path = "file_undo/ordinary.rs"]
mod ordinary;
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn write(path: &Path, bytes: &str) {
    fs::write(path, bytes).unwrap_or_else(|error| panic!("fixture write: {error}"));
}
fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("fixture read: {error}"))
}
fn create_in(address: SocketAddr, workspace: &Path) -> String {
    json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({"location":{"directory":workspace}}).to_string(),
    )["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("Session identity is absent"))
        .to_owned()
}
fn prompt(address: SocketAddr, session: &str, input: &str) {
    json(address, "POST", &format!("/api/session/{session}/prompt"), &serde_json::json!({"id":input,"text":"apply these patch changes","delivery":"queue","resume":true}).to_string());
}
fn preview(address: SocketAddr, session: &str) -> serde_json::Value {
    let path = format!("/api/session/{session}/undo/preview");
    let bound = super::client_allowance::CLI_REQUEST_ALLOWANCE;
    let mut selected = None;
    // The active entry can clear just before the workspace wake permit drops.
    // Wait for undo's own idle boundary, keeping every other refusal fatal.
    permission_residuals::wait_until(bound, permission_residuals::SETTLE_POLL, || {
        let (status, bytes) = request(address, "POST", &path, "{}");
        let response: serde_json::Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("undo preview response was not JSON: {error}"));
        if status == 200 {
            selected = Some(response);
            return true;
        }
        assert_eq!(status, 409, "{path}: {response}");
        assert_eq!(response["_tag"], "ConflictError", "{path}: {response}");
        assert_eq!(
            response["message"],
            "this workspace or a nested workspace has admitted work or file undo in progress",
            "{path}: {response}"
        );
        false
    })
    .unwrap_or_else(|| panic!("undo workspace did not settle within {bound:?}"));
    selected.unwrap_or_else(|| panic!("settled undo preview is absent"))
}
fn undo_body(preview: &serde_json::Value) -> String {
    serde_json::json!({"inputID":preview["data"]["inputID"],"target":preview["data"]["target"]})
        .to_string()
}
fn history(address: SocketAddr, session: &str) -> serde_json::Value {
    json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    )
}
fn workspace(label: &str) -> PathBuf {
    let path = root(label);
    process_fixture::cleanup(&path);
    fs::create_dir_all(&path).unwrap_or_else(|error| panic!("workspace creation: {error}"));
    // A separate plain workspace keeps the service journal outside its capture
    // root and does not invent a Git commit for the checkpoint foundation.
    path
}
fn git(workspace: &Path, arguments: &[&str]) -> String {
    let mut command = Command::new("git");
    command.env("GIT_CEILING_DIRECTORIES", std::env::temp_dir());
    command.arg("-C").arg(workspace).args(arguments);
    let (code, stdout, stderr) = super::project_list::bounded(command, Duration::from_secs(30));
    assert_eq!(code, Some(0), "fixture Git refused: {stderr}");
    stdout.trim().to_owned()
}

fn update(path: &str, before: &str, after: &str) -> serde_json::Value {
    serde_json::json!({"op":"update","path":path,"before_sha256":hash(before.as_bytes()),"old_text":before,"new_text":after})
}
#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn pu_native_file_undo_preserves_human_edits_and_reconciles_after_restart() {
    let _journey = process_journey_custody();
    let service = root("undo-positive-service");
    prepare(&service, 120_000);
    attended(&service);
    let workspace = workspace("undo-positive-workspace");
    write(&workspace.join("modify.txt"), "before");
    write(&workspace.join("delete.txt"), "deleted bytes");
    write(&workspace.join("move.txt"), "moved bytes");
    write(&workspace.join("human.txt"), "unrelated human data");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            workspace.join("modify.txt"),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap_or_else(|error| panic!("fixture mode: {error}"));
    }
    git(&workspace, &["init", "--quiet"]);
    git(
        &workspace,
        &[
            "add",
            "--",
            "modify.txt",
            "delete.txt",
            "move.txt",
            "human.txt",
        ],
    );
    git(
        &workspace,
        &[
            "-c",
            "user.name=Undo fixture",
            "-c",
            "user.email=undo-fixture@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "seed",
            "--",
            "modify.txt",
            "delete.txt",
            "move.txt",
            "human.txt",
        ],
    );
    let captured_head = git(&workspace, &["rev-parse", "HEAD"]);
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_undo_first",
            tool: "patch",
            arguments: serde_json::json!({"operations":[update("modify.txt", "before", "middle"), {"op":"create","path":"created.txt","content":"created bytes"}]}),
        },
        Script::ToolInput {
            call: "call_undo_second",
            tool: "patch",
            arguments: serde_json::json!({"operations":[update("modify.txt", "middle", "after"), {"op":"delete","path":"delete.txt","before_sha256":hash(b"deleted bytes")}, {"op":"rename","from":"move.txt","to":"moved.txt","before_sha256":hash(b"moved bytes")}]}),
        },
        Script::Success,
    ]);
    let mut child = spawn(&service, provider.address);
    let address = await_ready(&service, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create_in(address, &workspace);
    prompt(address, &session, "msg_file_undo_positive");
    compaction_continuity::approve_batch(
        address,
        &session,
        &["call_undo_first", "call_undo_second"],
    );
    wait_calls(&provider.calls, 3);
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(read(&workspace.join("modify.txt")), "after");
    assert!(workspace.join("created.txt").is_file());
    assert!(!workspace.join("delete.txt").exists());
    let mut before_history = history(address, &session);
    let selected = preview(address, &session);
    assert_eq!(selected["data"]["paths"].as_array().map(Vec::len), Some(5));
    assert_eq!(selected["data"]["writes"], 3);
    assert_eq!(selected["data"]["deletes"], 2);
    let body = undo_body(&selected);
    let path = format!("/api/session/{session}/undo");
    write(&workspace.join("branch-marker.txt"), "unrelated Git commit");
    git(&workspace, &["add", "--", "branch-marker.txt"]);
    git(
        &workspace,
        &[
            "-c",
            "user.name=Undo fixture",
            "-c",
            "user.email=undo-fixture@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "move baseline",
            "--",
            "branch-marker.txt",
        ],
    );
    assert_ne!(git(&workspace, &["rev-parse", "HEAD"]), captured_head);
    let baseline_refusal = json(address, "POST", &path, &body);
    assert_eq!(
        baseline_refusal["data"]["status"], "refused",
        "undo ignored its captured Git baseline"
    );
    assert!(workspace.join("created.txt").exists());
    assert_eq!(read(&workspace.join("modify.txt")), "after");
    git(&workspace, &["reset", "--soft", &captured_head]);
    write(&workspace.join("modify.txt"), "later human edit");
    for route in [&path, &format!("/api/session/{session}/undo/preview")] {
        let (status, response) = request(
            address,
            "POST",
            route,
            if route.ends_with("/preview") {
                "{}"
            } else {
                &body
            },
        );
        assert_eq!(
            status, 409,
            "undo must refuse a later human edit before any restore"
        );
        let refusal: serde_json::Value = serde_json::from_slice(&response)
            .unwrap_or_else(|error| panic!("undo conflict is not JSON: {error}"));
        let message = refusal["message"]
            .as_str()
            .unwrap_or_else(|| panic!("undo conflict has no message: {refusal}"));
        assert!(message.contains("files changed"), "{refusal}");
        assert!(message.contains("Inspect your edits"), "{refusal}");
    }
    assert_eq!(read(&workspace.join("modify.txt")), "later human edit");
    assert!(
        workspace.join("created.txt").exists(),
        "a precondition refusal partially restored another path"
    );
    write(&workspace.join("modify.txt"), "after");
    let receipt = json(address, "POST", &path, &body);
    assert_eq!(receipt["data"]["status"], "applied");
    assert_eq!(read(&workspace.join("modify.txt")), "before");
    assert_eq!(read(&workspace.join("delete.txt")), "deleted bytes");
    assert_eq!(read(&workspace.join("move.txt")), "moved bytes");
    assert!(!workspace.join("created.txt").exists());
    assert!(!workspace.join("moved.txt").exists());
    assert_eq!(read(&workspace.join("human.txt")), "unrelated human data");
    assert_eq!(
        history(address, &session),
        before_history,
        "file undo changed conversation history"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(workspace.join("modify.txt"))
                .unwrap_or_else(|error| panic!("restored metadata: {error}"))
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
    }
    write(&workspace.join("modify.txt"), "human edit after undo");
    // Conversation rewind hides the original prompt, while the independent
    // patch receipt and completed restore remain physical history.
    let cut = json(
        address,
        "POST",
        &format!("/api/session/{session}/rewind/preview"),
        r#"{"prompts":1}"#,
    );
    json(
        address,
        "POST",
        &format!("/api/session/{session}/rewind"),
        &serde_json::json!({"messageID":cut["data"]["messageID"],"tip":cut["data"]["tip"]})
            .to_string(),
    );
    assert_eq!(read(&workspace.join("modify.txt")), "human edit after undo");
    before_history = history(address, &session);
    child.stop_after_witness(&service);
    drop(child);
    let mut child = spawn(&service, provider.address);
    let address = await_ready(&service, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    assert_eq!(json(address, "POST", &path, &body), receipt);
    assert_eq!(
        read(&workspace.join("modify.txt")),
        "human edit after undo",
        "stable retry applied the restore again"
    );
    assert_eq!(history(address, &session), before_history);
    child.stop_after_witness(&service);
    process_fixture::cleanup(&workspace);
    process_fixture::cleanup(&service);
}

#[test]
fn pu_restore_process_loss_quarantines_partial_effects_without_reapplying() {
    let _journey = process_journey_custody();
    let service = root("undo-crash-service");
    prepare(&service, 120_000);
    attended(&service);
    let workspace = workspace("undo-crash-workspace");
    write(&workspace.join("a.txt"), "a before");
    write(&workspace.join("b.txt"), "b before");
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_undo_crash",
            tool: "patch",
            arguments: serde_json::json!({"operations":[update("a.txt", "a before", "a after"),update("b.txt", "b before", "b after")]}),
        },
        Script::Success,
    ]);
    let mut child =
        process_fixture::spawn_with(&service, provider.address, &[("GRAPT_PU_UNDO_PAUSE", "1")]);
    let address = await_ready(&service, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create_in(address, &workspace);
    prompt(address, &session, "msg_file_undo_crash");
    compaction_continuity::approve(address, &session, "call_undo_crash");
    wait_calls(&provider.calls, 2);
    permission_residuals::wait_session_idle(address, &session);
    let body = undo_body(&preview(address, &session));
    let path = format!("/api/session/{session}/undo");
    let mut stream =
        TcpStream::connect(address).unwrap_or_else(|error| panic!("undo connect: {error}"));
    stream.write_all(format!("POST {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: {AUTHORIZATION}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).unwrap_or_else(|error| panic!("undo send: {error}"));
    let mut entered = false;
    let wait = PollDeadline::new();
    while wait.pending() {
        if workspace
            .join(".grapt-pu-undo-entered")
            .try_exists()
            .unwrap_or_else(|error| panic!("restore marker: {error}"))
        {
            entered = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        entered,
        "restore never reached its first applied file ({wait})"
    );
    assert_eq!(read(&workspace.join("a.txt")), "a before");
    assert_eq!(read(&workspace.join("b.txt")), "b after");
    child.stop_after_witness(&service);
    drop(stream);
    drop(child);
    write(&workspace.join("a.txt"), "human repair after crash");
    let mut child = spawn(&service, provider.address);
    let address = await_ready(&service, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let quarantined = json(address, "POST", &path, &body);
    assert_eq!(quarantined["data"]["status"], "quarantined");
    assert_eq!(json(address, "POST", &path, &body), quarantined);
    assert_eq!(read(&workspace.join("a.txt")), "human repair after crash");
    assert_eq!(
        read(&workspace.join("b.txt")),
        "b after",
        "recovery replayed an unfinished restore"
    );
    child.stop_after_witness(&service);
    process_fixture::cleanup(&workspace);
    process_fixture::cleanup(&service);
}

#[test]
fn pu_partial_patch_undo_restores_only_confirmed_operations() {
    let _journey = process_journey_custody();
    let service = root("undo-partial-service");
    prepare(&service, 120_000);
    attended(&service);
    let workspace = workspace("undo-partial-workspace");
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_undo_partial",
            tool: "patch",
            arguments: serde_json::json!({"operations":[
                {"op":"create","path":"a.txt","content":"confirmed agent create"},
                {"op":"create","path":"b.txt","content":"must not replace human"},
            ]}),
        },
        Script::Success,
    ]);
    let mut child = process_fixture::spawn_with(
        &service,
        provider.address,
        &[("GRAPT_NT_PATCH_INTERRUPTION", "1")],
    );
    let address = await_ready(&service, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create_in(address, &workspace);
    prompt(address, &session, "msg_file_undo_partial");
    compaction_continuity::approve(address, &session, "call_undo_partial");
    let mut entered = false;
    let wait = PollDeadline::new();
    while wait.pending() {
        if workspace
            .join(".grapt-nt-patch-entered")
            .try_exists()
            .unwrap_or_else(|error| panic!("partial patch marker: {error}"))
        {
            entered = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(entered, "patch never reached its second operation ({wait})");
    write(&workspace.join("b.txt"), "concurrent human create");
    write(&workspace.join(".grapt-nt-patch-release"), "release");
    wait_calls(&provider.calls, 2);
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(read(&workspace.join("a.txt")), "confirmed agent create");
    assert_eq!(read(&workspace.join("b.txt")), "concurrent human create");
    let selected = preview(address, &session);
    assert_eq!(selected["data"]["paths"], serde_json::json!(["a.txt"]));
    assert_eq!(selected["data"]["incompletePatches"], 1);
    let undone = json(
        address,
        "POST",
        &format!("/api/session/{session}/undo"),
        &undo_body(&selected),
    );
    assert_eq!(undone["data"]["status"], "applied");
    assert!(!workspace.join("a.txt").exists());
    assert_eq!(
        read(&workspace.join("b.txt")),
        "concurrent human create",
        "undo attributed an incomplete operation to the agent"
    );
    child.stop_after_witness(&service);
    process_fixture::cleanup(&workspace);
    process_fixture::cleanup(&service);
}

#[test]
fn pu_partial_text_patch_undo_restores_what_its_applied_hunks_wrote() {
    let _journey = process_journey_custody();
    // The third hunk updates source.txt again but never runs, so the restore
    // target is the first hunk's bytes, not the plan's final postimage.
    let (service, workspace, mut child, _provider, address, session) = interrupted_text_patch(
        "undo-partial-text",
        |workspace| write(&workspace.join("source.txt"), "old\n"),
        "*** Begin Patch\n*** Update File: source.txt\n@@\n-old\n+new\n*** Add File: b.txt\n+must not replace human\n*** Update File: source.txt\n@@\n-new\n+newer\n*** End Patch",
        |workspace| write(&workspace.join("b.txt"), "concurrent human create"),
    );
    assert_eq!(read(&workspace.join("source.txt")), "new\n");
    assert_eq!(read(&workspace.join("b.txt")), "concurrent human create");
    let selected = preview(address, &session);
    assert_eq!(selected["data"]["paths"], serde_json::json!(["source.txt"]));
    assert_eq!(selected["data"]["incompletePatches"], 1);
    let undone = json(
        address,
        "POST",
        &format!("/api/session/{session}/undo"),
        &undo_body(&selected),
    );
    assert_eq!(undone["data"]["status"], "applied", "{undone}");
    assert_eq!(read(&workspace.join("source.txt")), "old\n");
    assert_eq!(
        read(&workspace.join("b.txt")),
        "concurrent human create",
        "undo attributed an incomplete hunk to the agent"
    );
    child.stop_after_witness(&service);
    process_fixture::cleanup(&workspace);
    process_fixture::cleanup(&service);
}

#[cfg(unix)]
#[test]
fn pu_patch_capture_preserves_human_mode_changes_between_operations() {
    use std::os::unix::fs::PermissionsExt;
    let _journey = process_journey_custody();
    for operation in ["delete", "rename", "update"] {
        let service = root(&format!("undo-mode-{operation}-service"));
        prepare(&service, 120_000);
        attended(&service);
        let workspace = workspace(&format!("undo-mode-{operation}-workspace"));
        let source = workspace.join("source.txt");
        write(&source, "source bytes");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o640))
            .unwrap_or_else(|error| panic!("fixture mode: {error}"));
        let change = match operation {
            "delete" => {
                serde_json::json!({"op":"delete","path":"source.txt","before_sha256":hash(b"source bytes")})
            }
            "rename" => {
                serde_json::json!({"op":"rename","from":"source.txt","to":"renamed.txt","before_sha256":hash(b"source bytes")})
            }
            _ => update("source.txt", "source bytes", "agent bytes"),
        };
        let provider = ProviderFixture::start(vec![
            Script::ToolInput {
                call: "call_undo_mode",
                tool: "patch",
                arguments: serde_json::json!({"operations":[
                    {"op":"create","path":"a.txt","content":"confirmed agent create"},
                    change,
                ]}),
            },
            Script::Success,
        ]);
        let mut child = process_fixture::spawn_with(
            &service,
            provider.address,
            &[("GRAPT_NT_PATCH_INTERRUPTION", "1")],
        );
        let address = await_ready(&service, &mut child);
        let _approver = approval::Attachment::open(address, "*");
        let session = create_in(address, &workspace);
        prompt(address, &session, "msg_file_undo_mode");
        compaction_continuity::approve(address, &session, "call_undo_mode");
        let mut entered = false;
        let wait = PollDeadline::new();
        while wait.pending() {
            if workspace
                .join(".grapt-nt-patch-entered")
                .try_exists()
                .unwrap_or_else(|error| panic!("mode barrier: {error}"))
            {
                entered = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(entered, "patch never reached its second operation ({wait})");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o600))
            .unwrap_or_else(|error| panic!("human chmod: {error}"));
        write(&workspace.join(".grapt-nt-patch-release"), "release");
        wait_calls(&provider.calls, 2);
        permission_residuals::wait_session_idle(address, &session);
        assert_eq!(
            read(&source),
            "source bytes",
            "patch applied after its source mode changed"
        );
        assert!(!workspace.join("renamed.txt").exists());
        let selected = preview(address, &session);
        assert_eq!(selected["data"]["paths"], serde_json::json!(["a.txt"]));
        assert_eq!(selected["data"]["incompletePatches"], 1);
        let restored = json(
            address,
            "POST",
            &format!("/api/session/{session}/undo"),
            &undo_body(&selected),
        );
        assert_eq!(restored["data"]["status"], "applied");
        assert!(!workspace.join("a.txt").exists());
        assert_eq!(read(&source), "source bytes");
        assert_eq!(
            fs::metadata(&source)
                .unwrap_or_else(|error| panic!("human metadata: {error}"))
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "undo discarded a concurrent human mode edit"
        );
        child.stop_after_witness(&service);
        process_fixture::cleanup(&workspace);
        process_fixture::cleanup(&service);
    }
}

/// Run a text patch that pauses before its second step, let `between` act
/// as the human, and return the settled service for undo assertions.
fn interrupted_text_patch(
    label: &str,
    seed: impl FnOnce(&Path),
    patch: &str,
    between: impl FnOnce(&Path),
) -> (
    PathBuf,
    PathBuf,
    process_fixture::ManagedChild,
    ProviderFixture,
    SocketAddr,
    String,
) {
    let service = root(&format!("{label}-service"));
    prepare(&service, 120_000);
    attended(&service);
    let workspace = workspace(&format!("{label}-workspace"));
    seed(&workspace);
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_undo_text_step",
            tool: "apply_patch",
            arguments: serde_json::json!({"input": patch}),
        },
        Script::Success,
    ]);
    let mut child = process_fixture::spawn_with(
        &service,
        provider.address,
        &[("GRAPT_NT_PATCH_INTERRUPTION", "1")],
    );
    let address = await_ready(&service, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create_in(address, &workspace);
    prompt(address, &session, "msg_file_undo_text_step");
    // Only an overwrite asks under this preset, so answer an ask if one
    // appears while waiting for the barrier rather than blocking on it.
    let mut entered = false;
    let wait = PollDeadline::new();
    while wait.pending() {
        if workspace
            .join(".grapt-nt-patch-entered")
            .try_exists()
            .unwrap_or_else(|error| panic!("text patch barrier: {error}"))
        {
            entered = true;
            break;
        }
        let asked = json(
            address,
            "GET",
            &format!("/api/session/{session}/permission"),
            "",
        );
        for row in asked["data"].as_array().into_iter().flatten() {
            if let Some(id) = row["id"].as_str() {
                let path = format!("/api/session/{session}/permission/{id}/reply");
                assert_eq!(
                    request(address, "POST", &path, r#"{"reply":"once"}"#).0,
                    204
                );
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(entered, "text patch never reached its second step ({wait})");
    between(&workspace);
    write(&workspace.join(".grapt-nt-patch-release"), "release");
    wait_calls(&provider.calls, 2);
    permission_residuals::wait_session_idle(address, &session);
    (service, workspace, child, provider, address, session)
}

#[cfg(unix)]
fn file_mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .unwrap_or_else(|error| panic!("fixture metadata: {error}"))
        .permissions()
        .mode()
        & 0o777
}

#[cfg(unix)]
#[test]
fn pu_text_patch_later_hunk_refuses_a_human_mode_change_and_undo_keeps_it() {
    use std::os::unix::fs::PermissionsExt;
    let _journey = process_journey_custody();
    let (service, workspace, mut child, _provider, address, session) = interrupted_text_patch(
        "undo-text-mode",
        |workspace| {
            write(&workspace.join("source.txt"), "old\n");
            fs::set_permissions(
                workspace.join("source.txt"),
                fs::Permissions::from_mode(0o640),
            )
            .unwrap_or_else(|error| panic!("fixture mode: {error}"));
        },
        "*** Begin Patch\n*** Update File: source.txt\n@@\n-old\n+middle\n*** Update File: source.txt\n@@\n-middle\n+final\n*** End Patch",
        |workspace| {
            fs::set_permissions(
                workspace.join("source.txt"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap_or_else(|error| panic!("human chmod: {error}"));
        },
    );
    let source = workspace.join("source.txt");
    assert_eq!(
        read(&source),
        "middle\n",
        "a later hunk applied after the human changed the file's mode"
    );
    // The file no longer matches what the patch left, so undo refuses at
    // preview rather than restoring the patch's mode over the human's.
    let (status, body) = request(
        address,
        "POST",
        &format!("/api/session/{session}/undo/preview"),
        "{}",
    );
    assert_eq!(status, 409, "{}", String::from_utf8_lossy(&body));
    assert_eq!(read(&source), "middle\n");
    assert_eq!(
        file_mode(&source),
        0o600,
        "undo discarded a concurrent human mode edit"
    );
    child.stop_after_witness(&service);
    process_fixture::cleanup(&workspace);
    process_fixture::cleanup(&service);
}

#[cfg(unix)]
#[test]
fn pu_text_patch_move_that_overwrote_its_destination_before_failing_is_undone() {
    use std::os::unix::fs::PermissionsExt;
    let _journey = process_journey_custody();
    let (service, workspace, mut child, _provider, address, session) = interrupted_text_patch(
        "undo-text-move",
        |workspace| {
            fs::create_dir(workspace.join("locked"))
                .unwrap_or_else(|error| panic!("fixture directory: {error}"));
            write(&workspace.join("locked/source.txt"), "old\n");
            write(&workspace.join("dest.txt"), "previous destination\n");
        },
        "*** Begin Patch\n*** Add File: a.txt\n+confirmed agent create\n*** Update File: locked/source.txt\n*** Move to: dest.txt\n@@\n-old\n+new\n*** End Patch",
        |workspace| {
            // The move can write its destination but not remove its source.
            fs::set_permissions(workspace.join("locked"), fs::Permissions::from_mode(0o555))
                .unwrap_or_else(|error| panic!("human chmod: {error}"));
        },
    );
    let unlock = || {
        fs::set_permissions(workspace.join("locked"), fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|error| panic!("fixture unlock: {error}"));
    };
    assert_eq!(read(&workspace.join("dest.txt")), "new\n");
    assert_eq!(read(&workspace.join("locked/source.txt")), "old\n");
    let selected = preview(address, &session);
    assert_eq!(
        selected["data"]["paths"],
        serde_json::json!(["a.txt", "dest.txt"]),
        "the move's confirmed destination write was not attributed"
    );
    assert_eq!(selected["data"]["incompletePatches"], 1);
    let undone = json(
        address,
        "POST",
        &format!("/api/session/{session}/undo"),
        &undo_body(&selected),
    );
    assert_eq!(undone["data"]["status"], "applied", "{undone}");
    assert!(!workspace.join("a.txt").exists());
    assert_eq!(read(&workspace.join("dest.txt")), "previous destination\n");
    assert_eq!(read(&workspace.join("locked/source.txt")), "old\n");
    unlock();
    child.stop_after_witness(&service);
    process_fixture::cleanup(&workspace);
    process_fixture::cleanup(&service);
}
