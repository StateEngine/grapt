//! Child placement through the real spawn tool and subsequent file tools.
use super::*;

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
        .current_dir(root)
        .args([
            "-c",
            if cfg!(windows) {
                "core.hooksPath=NUL"
            } else {
                "core.hooksPath=/dev/null"
            },
        ])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("fixture Git failed: {error}"));
    assert!(
        output.status.success(),
        "fixture Git refused: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn prompt(address: SocketAddr, session: &str) {
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"inspect the workspace"}"#,
    );
}

fn child_read(body: &str) -> Script {
    let request: serde_json::Value = serde_json::from_str(body)
        .unwrap_or_else(|error| panic!("invalid provider request: {error}"));
    let input = request["input"]
        .as_array()
        .unwrap_or_else(|| panic!("provider request omitted input"));
    let child = input.iter().any(|item| {
        item["role"] == "user" && item["content"].to_string().contains("WORKTREE_CHILD_INPUT")
    });
    let read = input
        .iter()
        .any(|item| item["type"] == "function_call_output" && item["call_id"] == "call_child_read");
    if child && !read {
        Script::ToolRead {
            call: "call_child_read",
            path: "file.txt",
        }
    } else {
        Script::Success
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn spawned_worktree_uses_committed_nested_cwd_and_parent_keeps_dirty_files() {
    let _journey = process_journey_custody();
    let root = root("agent-worktree");
    prepare(&root, 200_000);
    attended(&root);
    let source = root.join("repository");
    fs::create_dir(&source).unwrap_or_else(|error| panic!("{error}"));
    git(&source, &["init", "--quiet"]);
    fs::create_dir(source.join("nested")).unwrap_or_else(|error| panic!("{error}"));
    fs::write(source.join("nested/file.txt"), "committed child content")
        .unwrap_or_else(|error| panic!("{error}"));
    git(&source, &["add", "nested/file.txt"]);
    git(
        &source,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "initial",
        ],
    );
    fs::write(source.join("nested/file.txt"), "dirty parent content")
        .unwrap_or_else(|error| panic!("{error}"));
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_spawn",
            tool: "spawn_agent",
            arguments: serde_json::json!({"task_name":"inspect", "message":"WORKTREE_CHILD_INPUT", "worktree":true}),
        },
        Script::ForRequest(child_read),
        Script::ForRequest(child_read),
        Script::ForRequest(child_read),
    ]);
    let mut process = spawn(&root, provider.address);
    let address = await_ready(&root, &mut process);
    let _approver = approval::Attachment::open(address, "*");
    let created = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({"location":{"directory":source.join("nested")}}).to_string(),
    );
    let parent = created["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("session missing: {created}"));
    let parent_owned = parent.to_owned();
    let pending_prompt = thread::spawn(move || prompt(address, &parent_owned));
    let ask = permission_boundaries::pending(address, parent, "call_spawn");
    assert!(
        !root.join("worktrees").exists(),
        "checkout created before permission"
    );
    permission_boundaries::reply(address, parent, &ask, "once");
    pending_prompt
        .join()
        .unwrap_or_else(|_| panic!("spawn prompt failed"));
    permission_residuals::wait_session_idle(address, parent);
    let output = permission_boundaries::output(&provider, "call_spawn");
    let result: serde_json::Value = serde_json::from_str(&output)
        .unwrap_or_else(|error| panic!("invalid spawn result: {error}: {output}"));
    let child = result["agent_id"]
        .as_str()
        .unwrap_or_else(|| panic!("child omitted: {result}"));
    permission_residuals::wait_session_idle(address, child);
    let directory = PathBuf::from(
        result["directory"]
            .as_str()
            .unwrap_or_else(|| panic!("placement omitted: {result}")),
    );
    assert!(directory.ends_with("nested"));
    assert!(fs::canonicalize(&directory)
        .unwrap_or_else(|error| panic!("{error}"))
        .starts_with(
            fs::canonicalize(root.join("worktrees")).unwrap_or_else(|error| panic!("{error}"))
        ));
    assert!(directory
        .parent()
        .unwrap_or_else(|| panic!("checkout root missing"))
        .join(".git")
        .is_file());
    let record = json(address, "GET", &format!("/api/session/{child}"), "");
    assert_eq!(record["data"]["location"]["directory"], result["directory"]);
    assert_eq!(record["data"]["parentID"], parent);
    // A child accepts exactly one prompt; its initial execution already read
    // the relative file. The parent then reads the same relative path.
    assert!(permission_boundaries::output(&provider, "call_child_read")
        .contains("committed child content"));
    provider.append(vec![
        Script::ToolRead {
            call: "call_parent_read",
            path: "file.txt",
        },
        Script::Success,
    ]);
    prompt(address, parent);
    permission_residuals::wait_session_idle(address, parent);
    assert!(permission_boundaries::output(&provider, "call_parent_read")
        .contains("dirty parent content"));
    for session in [parent, child] {
        let history = json(
            address,
            "GET",
            &format!("/api/session/{session}/message"),
            "",
        );
        let rows = history["data"]
            .as_array()
            .unwrap_or_else(|| panic!("session history missing: {history}"));
        assert!(rows
            .iter()
            .all(|row| row["metadata"]["executionFailure"] != true));
        assert!(history
            .to_string()
            .contains("Hello from the live provider."));
    }
    assert_eq!(
        fs::read_to_string(source.join("nested/file.txt"))
            .unwrap_or_else(|error| panic!("{error}")),
        "dirty parent content"
    );
    process.stop_after_witness(&root);
    drop(process);
    process_fixture::cleanup(&root);
}
