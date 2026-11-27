//! Controlled real-product/CLI counterexamples for the permission boundary.
use super::*;
use sha2::{Digest, Sha256};

fn input(call: &'static str, tool: &'static str, arguments: serde_json::Value) -> Script {
    Script::ToolInput {
        call,
        tool,
        arguments,
    }
}

fn hash(path: &Path) -> String {
    let bytes = fs::read(path).unwrap_or_else(|error| panic!("fixture hash read failed: {error}"));
    format!("{:x}", Sha256::digest(bytes))
}

fn profile(root: &Path, preset: &str) {
    let path = root.join("service.conf");
    let mut config = fs::read_to_string(&path).unwrap_or_else(|error| panic!("{error}"));
    config.push_str(&format!("permission_preset={preset}\n"));
    fs::write(path, config).unwrap_or_else(|error| panic!("{error}"));
}

pub(super) fn pending(address: SocketAddr, session: &str, call: &str) -> serde_json::Value {
    let wait = PollDeadline::new();
    while wait.pending() {
        let page = json(
            address,
            "GET",
            &format!("/api/session/{session}/permission"),
            "",
        );
        if let Some(row) = page["data"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["source"]["callID"] == call))
        {
            return row.clone();
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("named permission boundary ask never appeared: {call} ({wait})");
}

pub(super) fn reply(address: SocketAddr, session: &str, row: &serde_json::Value, decision: &str) {
    let id = row["id"]
        .as_str()
        .unwrap_or_else(|| panic!("permission omitted its id"));
    let (status, body) = request(
        address,
        "POST",
        &format!("/api/session/{session}/permission/{id}/reply"),
        &serde_json::json!({"reply":decision}).to_string(),
    );
    assert_eq!(
        status,
        204,
        "permission reply failed: {}",
        String::from_utf8_lossy(&body)
    );
}

fn prompt(address: SocketAddr, session: &str) -> thread::JoinHandle<()> {
    let path = format!("/api/session/{session}/prompt");
    thread::spawn(move || {
        json(
            address,
            "POST",
            &path,
            r#"{"text":"exercise the permission boundary"}"#,
        );
    })
}

fn finish_prompt(prompt: thread::JoinHandle<()>) {
    prompt
        .join()
        .unwrap_or_else(|_| panic!("permission boundary prompt failed"));
}

fn auto(root: &Path, session: &str, mode: &str) -> super::cli_continuity::CliRun {
    let service = root
        .to_str()
        .unwrap_or_else(|| panic!("fixture path is not UTF-8"));
    super::cli_continuity::cli(
        root,
        &[
            "run",
            "--service",
            service,
            "--session",
            session,
            "--prompt",
            "exercise the permission boundary",
            mode,
            "--jsonl",
        ],
    )
}

pub(super) fn output(provider: &ProviderFixture, call: &str) -> String {
    locked(&provider.bodies)
        .iter()
        .find_map(|body| {
            let value: serde_json::Value = serde_json::from_str(body)
                .unwrap_or_else(|error| panic!("provider request is invalid: {error}"));
            value["input"]
                .as_array()
                .and_then(|items| {
                    items.iter().find(|item| {
                        matches!(
                            item["type"].as_str(),
                            Some("function_call_output" | "custom_tool_call_output")
                        ) && item["call_id"] == call
                    })
                })
                .and_then(|item| item["output"].as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| panic!("the real provider never received output for {call}"))
}

#[test]
fn remembered_patch_approval_cannot_lift_one_authority_file_and_once_recovers() {
    let _journey = process_journey_custody();
    let root = root("preset-patch-owner");
    prepare(&root, 200_000);
    profile(&root, "author");
    let ordinary = root.join("ordinary.txt");
    fs::write(&ordinary, "before").unwrap_or_else(|error| panic!("{error}"));
    let ordinary = fs::canonicalize(ordinary).unwrap_or_else(|error| panic!("{error}"));
    let authority =
        fs::canonicalize(root.join("service.conf")).unwrap_or_else(|error| panic!("{error}"));
    let authority_before = fs::read(&authority).unwrap_or_else(|error| panic!("{error}"));
    let patch = serde_json::json!({"operations":[
        {"op":"update","path":"ordinary.txt","before_sha256":hash(&ordinary),"old_text":"before","new_text":"after"},
        {"op":"update","path":"service.conf","before_sha256":hash(&authority),"old_text":"permission_preset=author","new_text":"permission_preset=maintainer"}
    ]});
    let provider = ProviderFixture::start(vec![
        input("call_patch_owner_reject", "patch", patch.clone()),
        Script::Success,
        input("call_patch_owner_once", "patch", patch),
        Script::Success,
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);
    // Remember the concrete patch targets, then prove the ordinary one is covered.
    let seed = json(
        address,
        "POST",
        &format!("/api/session/{session}/permission"),
        &serde_json::json!({"action":"patch","resources":[ordinary,authority],
            "save":[ordinary,authority],"agent":"agt_live_headless1"})
        .to_string(),
    );
    reply(address, &session, &seed["data"], "session");
    let covered = json(
        address,
        "POST",
        &format!("/api/session/{session}/permission"),
        &serde_json::json!({"action":"patch","resources":[ordinary],"agent":"agt_live_headless1"})
            .to_string(),
    );
    assert_eq!(
        covered["data"]["effect"], "allow",
        "the remembered concrete patch approval is not active"
    );
    let running = prompt(address, &session);
    let row = pending(address, &session, "call_patch_owner_reject");
    let resources: std::collections::BTreeSet<String> =
        serde_json::from_value(row["resources"].clone())
            .unwrap_or_else(|error| panic!("patch resources are malformed: {error}"));
    let approved: std::collections::BTreeSet<String> =
        serde_json::from_value(serde_json::json!([ordinary, authority]))
            .unwrap_or_else(|error| panic!("approved resources are malformed: {error}"));
    assert_eq!(
        resources, approved,
        "patch requested different approval resources"
    );
    assert_eq!(
        row["metadata"]["policy"]["ownerFloor"], true,
        "patch authority member lost its owner floor"
    );
    reply(address, &session, &row, "reject");
    finish_prompt(running);
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(
        fs::read(&ordinary).unwrap_or_else(|error| panic!("{error}")),
        b"before"
    );
    assert_eq!(
        fs::read(&authority).unwrap_or_else(|error| panic!("{error}")),
        authority_before,
        "rejected patch changed its authority source"
    );
    let running = prompt(address, &session);
    let row = pending(address, &session, "call_patch_owner_once");
    reply(address, &session, &row, "once");
    finish_prompt(running);
    permission_residuals::wait_session_idle(address, &session);
    let ordinary_after = fs::read(&ordinary).unwrap_or_else(|error| panic!("{error}"));
    let authority_after = fs::read_to_string(&authority).unwrap_or_else(|error| panic!("{error}"));
    let calls = provider.calls.load(Ordering::SeqCst);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    assert_eq!(
        calls, 4,
        "the patch did not reject then recover through the provider"
    );
    assert_eq!(ordinary_after, b"after");
    assert!(authority_after.contains("permission_preset=maintainer"));
}

#[test]
fn auto_refuses_a_patch_with_delete_and_rename_without_partial_effects() {
    let _journey = process_journey_custody();
    let root = root("preset-auto-patch-removal");
    prepare(&root, 200_000);
    attended(&root);
    for name in ["ordinary.txt", "delete.txt", "rename.txt"] {
        fs::write(root.join(name), "before").unwrap_or_else(|error| panic!("{error}"));
    }
    let patch = serde_json::json!({"operations":[
        {"op":"update","path":"ordinary.txt","before_sha256":hash(&root.join("ordinary.txt")),"old_text":"before","new_text":"after"},
        {"op":"delete","path":"delete.txt","before_sha256":hash(&root.join("delete.txt"))},
        {"op":"rename","from":"rename.txt","to":"renamed.txt","before_sha256":hash(&root.join("rename.txt"))}
    ]});
    let provider = ProviderFixture::start(vec![
        input("call_auto_patch_reject", "patch", patch.clone()),
        Script::Success,
        input("call_skip_patch_once", "patch", patch),
        Script::Success,
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);
    let refused = auto(&root, &session, "--auto");
    assert_eq!(refused.code, Some(0), "{}", refused.stderr);
    assert!(
        refused.stderr.contains("refused patch"),
        "automatic reply approved a removal patch: {}",
        refused.stderr
    );
    for name in ["ordinary.txt", "delete.txt", "rename.txt"] {
        assert_eq!(
            fs::read(root.join(name)).unwrap_or_else(|error| panic!("{error}")),
            b"before",
            "automatic patch removal or partial write occurred"
        );
    }
    assert!(!root.join("renamed.txt").exists());
    let allowed = auto(&root, &session, "--dangerously-skip-permissions");
    let ordinary = fs::read(root.join("ordinary.txt")).unwrap_or_else(|error| panic!("{error}"));
    let removed = !root.join("delete.txt").exists() && !root.join("rename.txt").exists();
    let renamed = fs::read(root.join("renamed.txt")).unwrap_or_else(|error| panic!("{error}"));
    let calls = provider.calls.load(Ordering::SeqCst);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    assert_eq!(allowed.code, Some(0), "{}", allowed.stderr);
    assert_eq!(calls, 4);
    assert_eq!(ordinary, b"after");
    assert!(removed);
    assert_eq!(renamed, b"before");
}

#[test]
fn automatic_replies_never_edit_a_launch_explicit_authority_path() {
    let _journey = process_journey_custody();
    let root = root("preset-explicit-authority");
    prepare(&root, 200_000);
    let policy = root.join("policy.json");
    let before = r#"{"permission_preset":"author"}"#;
    fs::write(&policy, before).unwrap_or_else(|error| panic!("{error}"));
    let edit =
        serde_json::json!({"path":"policy.json","old_text":"author","new_text":"maintainer"});
    let provider = ProviderFixture::start(vec![
        input("call_policy_auto", "edit", edit.clone()),
        Script::Success,
        input("call_policy_skip", "edit", edit.clone()),
        Script::Success,
        input("call_policy_once", "edit", edit),
        Script::Success,
    ]);
    let mut child = process_fixture::spawn_with(
        &root,
        provider.address,
        &[(
            "GRAPT_CONFIG",
            policy
                .to_str()
                .unwrap_or_else(|| panic!("policy path is not UTF-8")),
        )],
    );
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);
    for mode in ["--auto", "--dangerously-skip-permissions"] {
        let result = auto(&root, &session, mode);
        assert_eq!(result.code, Some(0), "{}", result.stderr);
        assert!(
            result
                .stderr
                .contains("permission settings require an explicit decision"),
            "automatic reply failed to refuse a permission authority source: {}",
            result.stderr
        );
        assert_eq!(
            fs::read_to_string(&policy).unwrap_or_else(|error| panic!("{error}")),
            before,
            "automatic reply rewrote a resolved authority source"
        );
    }
    let running = prompt(address, &session);
    let row = pending(address, &session, "call_policy_once");
    assert_eq!(row["metadata"]["policy"]["ownerFloor"], true);
    reply(address, &session, &row, "once");
    finish_prompt(running);
    permission_residuals::wait_session_idle(address, &session);
    let after = fs::read_to_string(&policy).unwrap_or_else(|error| panic!("{error}"));
    let calls = provider.calls.load(Ordering::SeqCst);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    assert_eq!(calls, 6);
    assert!(
        after.contains("maintainer"),
        "an attended once reply could not recover the edit"
    );
}

#[test]
fn an_author_parent_cannot_delegate_a_maintainer_child() {
    let _journey = process_journey_custody();
    let root = root("preset-delegation-ceiling");
    prepare(&root, 200_000);
    let provider = ProviderFixture::start(vec![
        Script::ToolSubagent {
            call: "call_author_delegation",
            task: "try an execution",
        },
        input(
            "call_delegated_shell",
            "shell",
            serde_json::json!({"command":"echo delegated-escalation"}),
        ),
        Script::SuccessText("child ended"),
        Script::SuccessText("parent ended"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);
    let (status, body) = request(
        address,
        "POST",
        &format!("/api/session/{session}/preset"),
        r#"{"preset":"author"}"#,
    );
    assert_eq!(status, 204, "{}", String::from_utf8_lossy(&body));
    let result = auto(&root, &session, "--auto");
    let tool_output = output(&provider, "call_delegated_shell");
    let page = json(address, "GET", "/api/session", "");
    let children: Vec<_> = page["data"]
        .as_array()
        .unwrap_or_else(|| panic!("Session list is absent"))
        .iter()
        .filter(|row| row["parentID"] == session)
        .collect();
    assert_eq!(children.len(), 1);
    let child_id = children[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("child id absent"));
    let child_record = json(address, "GET", &format!("/api/session/{child_id}"), "");
    let calls = provider.calls.load(Ordering::SeqCst);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    assert_eq!(result.code, Some(0), "{}", result.stderr);
    assert_eq!(calls, 4);
    assert_eq!(
        child_record["data"]["preset"], "author",
        "child lost the inherited permission ceiling"
    );
    assert!(
        tool_output.contains("denied by the ancestor permission policy"),
        "child widened delegated permissions: {tool_output}"
    );
    assert!(
        !tool_output.contains("delegated-escalation"),
        "a delegated command ran: {tool_output}"
    );
}

#[test]
fn creating_an_absent_launch_authority_source_requires_an_explicit_decision() {
    let _journey = process_journey_custody();
    let root = root("preset-absent-authority");
    prepare(&root, 200_000);
    profile(&root, "author");
    let policy = root.join("missing-policy.json");
    let content = r#"{"permission_preset":"maintainer"}"#;
    let write = serde_json::json!({"path":"missing-policy.json","content":content});
    let provider = ProviderFixture::start(vec![
        input("call_missing_auto", "write", write.clone()),
        Script::Success,
        input("call_missing_once", "write", write),
        Script::Success,
    ]);
    let mut child = process_fixture::spawn_with(
        &root,
        provider.address,
        &[(
            "GRAPT_CONFIG",
            policy
                .to_str()
                .unwrap_or_else(|| panic!("policy path is not UTF-8")),
        )],
    );
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);
    let refused = auto(&root, &session, "--auto");
    assert_eq!(refused.code, Some(0), "{}", refused.stderr);
    assert!(
        refused
            .stderr
            .contains("permission settings require an explicit decision"),
        "{}",
        refused.stderr
    );
    assert!(
        !policy.exists(),
        "automatic reply created an authority source"
    );
    let running = prompt(address, &session);
    let row = pending(address, &session, "call_missing_once");
    assert_eq!(row["metadata"]["policy"]["ownerFloor"], true);
    reply(address, &session, &row, "once");
    finish_prompt(running);
    permission_residuals::wait_session_idle(address, &session);
    let after = fs::read_to_string(&policy).unwrap_or_else(|error| panic!("{error}"));
    let calls = provider.calls.load(Ordering::SeqCst);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    assert_eq!(calls, 4);
    assert_eq!(
        after, content,
        "explicit once did not recover authority creation"
    );
}

fn only_child(address: SocketAddr, parent: &str) -> String {
    let page = json(address, "GET", "/api/session", "");
    let children: Vec<_> = page["data"]
        .as_array()
        .unwrap_or_else(|| panic!("Session list is absent"))
        .iter()
        .filter(|row| row["parentID"].as_str() == Some(parent))
        .collect();
    assert_eq!(
        children.len(),
        1,
        "delegation did not create exactly one child"
    );
    children[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("child id is absent"))
        .to_owned()
}

fn select(address: SocketAddr, session: &str, preset: &str) {
    let (status, body) = request(
        address,
        "POST",
        &format!("/api/session/{session}/preset"),
        &serde_json::json!({"preset":preset}).to_string(),
    );
    assert_eq!(status, 204, "{}", String::from_utf8_lossy(&body));
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn a_child_override_cannot_widen_descendants_and_child_config_can_narrow() {
    let _journey = process_journey_custody();
    let root = root("preset-descendant-ceiling");
    prepare(&root, 200_000);
    fs::write(root.join("ordinary.txt"), "before").unwrap_or_else(|error| panic!("{error}"));
    let provider = ProviderFixture::start(vec![
        Script::ToolSubagent {
            call: "call_first_child",
            task: "read the repository",
        },
        Script::Success,
        Script::Success,
        Script::ToolSubagent {
            call: "call_grandchild",
            task: "try execution",
        },
        input(
            "call_grandchild_shell",
            "shell",
            serde_json::json!({"command":"echo descendant-escalation"}),
        ),
        Script::Success,
        Script::Success,
        input(
            "call_narrowed_child_edit",
            "edit",
            serde_json::json!({"path":"ordinary.txt","old_text":"before","new_text":"after"}),
        ),
        Script::Success,
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let parent = create(address);
    select(address, &parent, "author");
    let first = auto(&root, &parent, "--auto");
    assert_eq!(first.code, Some(0), "{}", first.stderr);
    let first_child = only_child(address, &parent);
    select(address, &first_child, "maintainer");
    assert_eq!(
        json(
            address,
            "GET",
            &format!("/api/session/{first_child}/preset"),
            ""
        )["data"],
        serde_json::json!({"selected":"maintainer","effective":"author"}),
        "preset display widened the ancestor ceiling"
    );
    select(address, &first_child, "bypass");
    assert_eq!(
        json(
            address,
            "GET",
            &format!("/api/session/{first_child}/preset"),
            ""
        )["data"],
        serde_json::json!({"selected":"bypass","effective":"author"}),
        "a child bypass selection widened its author parent"
    );
    select(address, &first_child, "maintainer");
    // A subagent child accepts one prompt. Fork its history to exercise a
    // fresh descendant while retaining the overridden child and root ceilings.
    let forked = json(
        address,
        "POST",
        &format!("/api/session/{first_child}/fork"),
        "{}",
    );
    let delegated = forked["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("descendant fork id is absent"))
        .to_owned();
    select(address, &delegated, "maintainer");
    let running = prompt(address, &delegated);
    let row = pending(address, &delegated, "call_grandchild");
    reply(address, &delegated, &row, "once");
    finish_prompt(running);
    permission_residuals::wait_session_idle(address, &delegated);
    let grandchild = only_child(address, &delegated);
    let grandchild_record = json(address, "GET", &format!("/api/session/{grandchild}"), "");
    let grandchild_output = output(&provider, "call_grandchild_shell");
    fs::write(
        root.join("grapt.json"),
        r#"{"permission_preset":"inspect"}"#,
    )
    .unwrap_or_else(|error| panic!("{error}"));
    let narrowed = auto(&root, &delegated, "--auto");
    let narrowed_output = output(&provider, "call_narrowed_child_edit");
    let narrowed_preset = json(
        address,
        "GET",
        &format!("/api/session/{delegated}/preset"),
        "",
    );
    let bytes = fs::read(root.join("ordinary.txt")).unwrap_or_else(|error| panic!("{error}"));
    let calls = provider.calls.load(Ordering::SeqCst);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    assert_eq!(
        grandchild_record["data"]["preset"], "author",
        "child override widened a descendant"
    );
    assert!(
        grandchild_output.contains("denied by the ancestor permission policy"),
        "{grandchild_output}"
    );
    assert!(
        !grandchild_output.contains("descendant-escalation"),
        "{grandchild_output}"
    );
    assert_eq!(narrowed.code, Some(0), "{}", narrowed.stderr);
    assert_eq!(
        narrowed_preset["data"],
        serde_json::json!({"selected":"maintainer","effective":"inspect"}),
        "project config did not narrow the overridden child's effective preset"
    );
    assert!(
        narrowed_output.contains("denied by the ancestor permission policy"),
        "child config failed to narrow: {narrowed_output}"
    );
    assert_eq!(bytes, b"before");
    assert_eq!(calls, 9, "descendant/narrowing witnesses did not run");
}

#[test]
fn editing_a_hard_link_alias_cannot_rewrite_an_authority_source() {
    let _journey = process_journey_custody();
    let root = root("preset-authority-alias");
    prepare(&root, 200_000);
    let policy = root.join("policy.json");
    let alias = root.join("ordinary.json");
    let before = r#"{"permission_preset":"author"}"#;
    fs::write(&policy, before).unwrap_or_else(|error| panic!("{error}"));
    fs::hard_link(&policy, &alias)
        .unwrap_or_else(|error| panic!("authority alias setup failed: {error}"));
    let provider = ProviderFixture::start(vec![
        input(
            "call_policy_alias_edit",
            "edit",
            serde_json::json!({"path":"ordinary.json","old_text":"author","new_text":"maintainer"}),
        ),
        Script::Success,
        input(
            "call_policy_alias_shell",
            "shell",
            serde_json::json!({"command":"echo alias-escalation"}),
        ),
        Script::Success,
    ]);
    let mut child = process_fixture::spawn_with(
        &root,
        provider.address,
        &[(
            "GRAPT_CONFIG",
            policy
                .to_str()
                .unwrap_or_else(|| panic!("policy path is not UTF-8")),
        )],
    );
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);
    let edit = auto(&root, &session, "--auto");
    assert_eq!(edit.code, Some(0), "{}", edit.stderr);
    assert_eq!(
        fs::read_to_string(&policy).unwrap_or_else(|error| panic!("{error}")),
        before,
        "a hard-link alias rewrote its authority source"
    );
    assert!(
        fs::read_to_string(&alias)
            .unwrap_or_else(|error| panic!("{error}"))
            .contains("maintainer"),
        "the ordinary alias was not edited"
    );
    let command = auto(&root, &session, "--auto");
    let shell_output = output(&provider, "call_policy_alias_shell");
    let calls = provider.calls.load(Ordering::SeqCst);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    assert_eq!(command.code, Some(0), "{}", command.stderr);
    assert!(
        shell_output.contains("denied by this Session's author permission preset"),
        "the next Step gained authority: {shell_output}"
    );
    assert!(!shell_output.contains("alias-escalation"), "{shell_output}");
    assert_eq!(calls, 4);
}

#[cfg(any(unix, windows))]
#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn file_and_parent_symlink_aliases_cannot_rewrite_authority_sources() {
    let _journey = process_journey_custody();
    let root = root("preset-authority-symlinks");
    prepare(&root, 200_000);
    let policy = root.join("policy.json");
    let before = r#"{"permission_preset":"author"}"#;
    fs::write(&policy, before).unwrap_or_else(|error| panic!("{error}"));
    let cases = [
        (
            "notes.json",
            false,
            [
                "call_file_alias_edit",
                "call_file_alias_write",
                "call_file_alias_patch",
            ],
        ),
        (
            "aliases",
            true,
            [
                "call_parent_alias_edit",
                "call_parent_alias_write",
                "call_parent_alias_patch",
            ],
        ),
    ];
    let mut scripts = Vec::new();
    for (name, directory, calls) in cases {
        let link = root.join(name);
        let target = if directory { &root } else { &policy };
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(target, &link);
        #[cfg(windows)]
        let linked = if directory {
            std::os::windows::fs::symlink_dir(target, &link)
        } else {
            std::os::windows::fs::symlink_file(target, &link)
        };
        linked.unwrap_or_else(|error| panic!("authority symlink setup failed: {error}"));
        let requested = if directory {
            format!("{name}/policy.json")
        } else {
            name.to_owned()
        };
        let arguments = [
            (
                "edit",
                serde_json::json!({"path":requested,"old_text":"author","new_text":"maintainer"}),
            ),
            (
                "write",
                serde_json::json!({"path":requested,"content":r#"{"permission_preset":"maintainer"}"#}),
            ),
            (
                "patch",
                serde_json::json!({"operations":[{"op":"update","path":requested,"before_sha256":hash(&policy),"old_text":"author","new_text":"maintainer"}]}),
            ),
        ];
        for (call, (tool, arguments)) in calls.into_iter().zip(arguments) {
            scripts.push(input(call, tool, arguments));
            scripts.push(Script::Success);
        }
    }
    let provider = ProviderFixture::start(scripts);
    let mut child = process_fixture::spawn_with(
        &root,
        provider.address,
        &[(
            "GRAPT_CONFIG",
            policy
                .to_str()
                .unwrap_or_else(|| panic!("policy path is not UTF-8")),
        )],
    );
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);
    for (_, _, calls) in cases {
        for (index, call) in calls.into_iter().enumerate() {
            let result = auto(&root, &session, "--auto");
            assert_eq!(result.code, Some(0), "{}", result.stderr);
            assert_eq!(
                fs::read_to_string(&policy).unwrap_or_else(|error| panic!("{error}")),
                before,
                "a symlink alias rewrote its authority source: {call}"
            );
            if index == 0 {
                assert!(
                    result
                        .stderr
                        .contains("permission settings require an explicit decision"),
                    "edit did not canonicalize its authority target: {}",
                    result.stderr
                );
            } else {
                let failure = output(&provider, call);
                assert!(
                    failure.contains("tool preparation failed")
                        && (failure.contains("symlink")
                            || failure.contains("write target exists and is not a regular file")),
                    "write/patch did not refuse the symlink before effects: {call}: {failure}"
                );
            }
        }
    }
    let calls = provider.calls.load(Ordering::SeqCst);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    assert_eq!(calls, 12);
}

/// The tool names one provider request offered the model, as it received them.
fn offered(body: &str) -> Vec<String> {
    let value: serde_json::Value = serde_json::from_str(body)
        .unwrap_or_else(|error| panic!("provider request is invalid: {error}"));
    value["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("provider request carried no tools: {body}"))
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect()
}

/// A tool the Session's permissions deny outright is never
/// offered to the model, as the oracle withholds it; read from the request the
/// provider received. One the model calls anyway is still refused, in the
/// preset's own words, and never runs. A maintainer Session is still offered
/// what inspect withholds.
#[test]
fn a_tool_denied_outright_is_not_offered_and_still_refused_if_called() {
    let _journey = process_journey_custody();
    let root = root("preset-withholds-denied-tools");
    prepare(&root, 200_000);
    let provider = ProviderFixture::start(vec![
        input(
            "call_withheld_shell",
            "shell",
            serde_json::json!({"command":"echo withheld-escalation"}),
        ),
        Script::SuccessText("inspect ended"),
        Script::SuccessText("maintainer ended"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let inspect = create(address);
    select(address, &inspect, "inspect");
    let narrowed = auto(&root, &inspect, "--auto");
    let maintainer = create(address);
    let control = auto(&root, &maintainer, "--auto");
    let refused = output(&provider, "call_withheld_shell");
    let bodies = locked(&provider.bodies).clone();
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    assert_eq!(narrowed.code, Some(0), "{}", narrowed.stderr);
    assert_eq!(control.code, Some(0), "{}", control.stderr);
    assert_eq!(bodies.len(), 3, "provider requests");
    let inspected = offered(&bodies[0]);
    for withheld in [
        "shell",
        "write",
        "edit",
        "patch",
        "apply_patch",
        "subagent",
        "skill",
        "execute",
    ] {
        assert!(
            !inspected.iter().any(|name| name == withheld),
            "inspect offered the denied {withheld}: {inspected:?}"
        );
    }
    for kept in ["read", "read_lines", "list", "grep", "glob"] {
        assert!(
            inspected.iter().any(|name| name == kept),
            "inspect withheld the allowed {kept}: {inspected:?}"
        );
    }
    let maintained = offered(&bodies[2]);
    for kept in ["shell", "write", "apply_patch", "read"] {
        assert!(
            maintained.iter().any(|name| name == kept),
            "maintainer lost {kept}: {maintained:?}"
        );
    }
    assert!(
        refused.contains("denied by this Session's inspect permission preset"),
        "{refused}"
    );
    assert!(!refused.contains("withheld-escalation"), "{refused}");
}
