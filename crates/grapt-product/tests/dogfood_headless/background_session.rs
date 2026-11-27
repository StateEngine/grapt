//! A real CLI exits while the service continues in an isolated checkout.

use super::*;

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
        .current_dir(root)
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

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn background_cli_exits_while_service_finishes_in_its_worktree() {
    let _journey = process_journey_custody();
    let root = root("background-worktree");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    let source = root.join("repository");
    fs::create_dir(&source).unwrap_or_else(|error| panic!("fixture mkdir: {error}"));
    git(&source, &["init", "--initial-branch=main"]);
    fs::write(source.join("file.txt"), "committed")
        .unwrap_or_else(|error| panic!("fixture write: {error}"));
    git(&source, &["add", "file.txt"]);
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
    fs::write(source.join("file.txt"), "local changes")
        .unwrap_or_else(|error| panic!("fixture write: {error}"));

    let entered = Arc::new(terminal_coordination::Phase::new());
    let release = Arc::new(terminal_coordination::Phase::new());
    let provider = ProviderFixture::start(vec![Script::HeldTerminalOutput {
        bytes: 32,
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    }]);
    let mut product = spawn(&root, provider.address);
    let address = await_ready(&root, &mut product);
    let destination = root.join("isolated checkout");
    let run = Command::new(super::shipped_cli::cli_binary())
        .current_dir(&source)
        .args(["run", "--service"])
        .arg(&root)
        .args(["--background", "--json", "--worktree"])
        .arg(&destination)
        .arg("hello")
        // A Git wrapper's inherited selector must not redirect our checkout.
        .env("GIT_DIR", root.join("not-a-repository"))
        .env("GIT_WORK_TREE", &root)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("background CLI launch: {error}"));
    assert!(
        run.status.success(),
        "background CLI failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&run.stdout).unwrap_or_else(|error| {
        panic!(
            "background receipt missing: {error}: {}",
            String::from_utf8_lossy(&run.stdout)
        )
    });
    assert_eq!(receipt["schema"], "grapt.cli.background-receipt.v1");
    let session = receipt["session"]
        .as_str()
        .unwrap_or_else(|| panic!("receipt has no session"));
    // The provider cannot finish until this client has already exited.
    entered
        .wait("background provider entry", &AtomicBool::new(false))
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        fs::read_to_string(source.join("file.txt"))
            .unwrap_or_else(|error| panic!("source read: {error}")),
        "local changes"
    );
    assert_eq!(
        fs::read_to_string(destination.join("file.txt"))
            .unwrap_or_else(|error| panic!("checkout read: {error}")),
        "committed"
    );
    let (status, body) = request(address, "GET", &format!("/api/session/{session}"), "");
    assert_eq!(status, 200);
    let record: serde_json::Value =
        serde_json::from_slice(&body).unwrap_or_else(|error| panic!("session response: {error}"));
    assert_eq!(
        fs::canonicalize(
            record["data"]["location"]["directory"]
                .as_str()
                .unwrap_or_else(|| panic!("session has no directory"))
        )
        .unwrap_or_else(|error| panic!("session directory: {error}")),
        fs::canonicalize(&destination)
            .unwrap_or_else(|error| panic!("checkout directory: {error}")),
    );
    release.release();
    permission_residuals::wait_session_idle(address, session);
    let (status, body) = request(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    );
    assert_eq!(status, 200);
    let messages: serde_json::Value =
        serde_json::from_slice(&body).unwrap_or_else(|error| panic!("messages: {error}"));
    let expected = format!(
        "{}{}",
        provider_fixture::OUTPUT_OBJECTIVE,
        "x".repeat(32 - provider_fixture::OUTPUT_OBJECTIVE.len())
    );
    let rows = messages["data"]
        .as_array()
        .unwrap_or_else(|| panic!("history is not a list: {messages}"));
    assert!(
        rows.iter().any(|row| {
            row["type"] == "assistant"
                && row["content"].as_array().is_some_and(|parts| {
                    parts.iter().any(|part| {
                        part["type"] == "text" && part["text"].as_str() == Some(&expected)
                    })
                })
        }),
        "completed background output is missing: {messages}"
    );
    assert!(
        destination.join(".git").is_file(),
        "checkout was removed after completion"
    );
    product.stop_after_witness(&root);
}
