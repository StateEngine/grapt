//! CLI-COMPATIBILITY-MATRIX-01: the TypeScript CLI forms this CLI implements,
//! driven through the shipped `grapt` binary against the real product.

use super::cli_continuity::cli;
use super::*;

/// Far below the fixture's 120 s serve window, which is how long a caller
/// reading through a pipe would wait if the started service held that pipe.
const CAPTURED_START_BOUND: Duration = Duration::from_secs(30);

fn stdout_line(run: &super::cli_continuity::CliRun) -> &str {
    run.stdout.strip_suffix('\n').unwrap_or(&run.stdout)
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn the_typescript_run_service_and_resume_forms_work_against_the_real_product() {
    let _journey = process_journey_custody();
    let root = root("cls-cli-compatibility");
    prepare(&root, 120_000);
    let service = root.to_string_lossy().into_owned();
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("first answer through the CLI"),
        Script::SuccessText("continued answer"),
        Script::SuccessText("embedded answer"),
    ]);
    let mut product = spawn(&root, provider.address);
    let address = await_ready(&root, &mut product);

    // `service status` prints the live service URL on one line.
    let status = cli(&root, &["service", "status", "--service", &service]);
    assert_eq!(
        status.code,
        Some(0),
        "service status failed: {}",
        status.stderr
    );
    let url = stdout_line(&status);
    assert!(
        url.trim_end_matches('/') == format!("http://{address}"),
        "service status printed {url:?}, not the live endpoint {address}"
    );

    // `run <message...>`, no selector: a new Session; stdout is the answer alone.
    let first = cli(&root, &["run", "--service", &service, "first", "task"]);
    assert_eq!(first.code, Some(0), "text run failed: {}", first.stderr);
    assert_eq!(first.stdout, "first answer through the CLI\n");
    assert!(
        first.stderr.contains("> "),
        "the step label did not reach stderr: {}",
        first.stderr
    );
    let session = cli(
        &root,
        &["attach", "--service", &service, "--resume", "--jsonl"],
    )
    .receipt()["session"]
        .as_str()
        .unwrap_or_else(|| panic!("attach --resume named no Session"))
        .to_owned();

    // `-c` continues that same Session.
    let continued = cli(
        &root,
        &["run", "--service", &service, "-c", "--jsonl", "go", "on"],
    );
    assert_eq!(
        continued.code,
        Some(0),
        "-c run failed: {}",
        continued.stderr
    );
    assert_eq!(continued.receipt()["session"], session.as_str());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);

    product.stop_after_witness(&root);
    drop(product);
    let stopped = cli(&root, &["service", "status", "--service", &service]);
    assert_eq!(
        stopped.code,
        Some(0),
        "stopped status failed: {}",
        stopped.stderr
    );
    assert_eq!(stopped.stdout, "stopped\n");

    // `service start` launches the product as the service owner and prints its URL.
    let product_binary = env!("CARGO_BIN_EXE_grapt-server");
    let asked = tokio::time::Instant::now();
    let started = with_provider(
        &root,
        &[
            "service",
            "start",
            "--service",
            &service,
            "--product",
            product_binary,
        ],
        provider.address,
    );
    let waited = asked.elapsed();
    assert_eq!(
        started.status.code(),
        Some(0),
        "service start failed: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    // Read through a pipe, `service start` returns once the service is up; the
    // service it leaves running must not hold the pipe its caller reads.
    assert!(
        waited < CAPTURED_START_BOUND,
        "a caller reading service start through a pipe waited {waited:?}"
    );
    let started_url = String::from_utf8_lossy(&started.stdout).into_owned();
    let ready: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("serve-ready.json"))
            .unwrap_or_else(|error| panic!("service start published no readiness: {error}")),
    )
    .unwrap_or_else(|error| panic!("readiness receipt is not JSON: {error}"));
    let pid = ready["pid"]
        .as_u64()
        .unwrap_or_else(|| panic!("readiness names no pid"));
    let status_after = cli(&root, &["service", "status", "--service", &service]);
    stop_started_service(pid, &root);
    let endpoint = ready["observed_endpoint"].as_str().unwrap_or_default();
    assert_eq!(
        started_url.trim_end_matches('\n').trim_end_matches('/'),
        endpoint.trim_end_matches('/'),
        "service start did not print the started service's URL"
    );
    assert_eq!(status_after.stdout, started_url);

    // `launch` is the same start, rendered as `launched <url>`.
    let asked = tokio::time::Instant::now();
    let launched = with_provider(
        &root,
        &["launch", "--service", &service, "--product", product_binary],
        provider.address,
    );
    let waited = asked.elapsed();
    assert_eq!(
        launched.status.code(),
        Some(0),
        "launch failed: {}",
        String::from_utf8_lossy(&launched.stderr)
    );
    assert!(
        waited < CAPTURED_START_BOUND,
        "a caller reading launch through a pipe waited {waited:?}"
    );
    let launched_ready: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("serve-ready.json"))
            .unwrap_or_else(|error| panic!("launch published no readiness: {error}")),
    )
    .unwrap_or_else(|error| panic!("readiness receipt is not JSON: {error}"));
    let launched_pid = launched_ready["pid"]
        .as_u64()
        .unwrap_or_else(|| panic!("readiness names no pid"));
    stop_started_service(launched_pid, &root);
    assert!(
        String::from_utf8_lossy(&launched.stdout).starts_with("launched http://"),
        "launch did not report its started service: {}",
        String::from_utf8_lossy(&launched.stdout)
    );

    // `--embedded` (the oracle's `--standalone`) runs against a private
    // product the CLI starts and ends.
    let embedded = with_provider(
        &root,
        &[
            "run",
            "--embedded",
            "--service",
            &service,
            "--product",
            product_binary,
            "-c",
            "--jsonl",
            "embedded",
            "turn",
        ],
        provider.address,
    );
    assert_eq!(
        embedded.status.code(),
        Some(0),
        "embedded run failed: {}",
        String::from_utf8_lossy(&embedded.stderr)
    );
    assert!(
        String::from_utf8_lossy(&embedded.stdout).contains("embedded answer"),
        "the embedded run did not reach its private product"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    drop(provider);
    process_fixture::cleanup(&root);
}

/// `run --fork` against the real product: the forked prompt executes and
/// settles in a new child of the selected Session, which carries the parent's
/// conversation, and the parent's history is left exactly as it was. This is
/// FORK-SESSION-EXECUTION-01 J03's CLI row.
#[test]
fn run_fork_executes_in_a_child_and_leaves_the_parent_unchanged() {
    let _journey = process_journey_custody();
    let root = root("ccm-cli-fork");
    prepare(&root, 120_000);
    let service = root.to_string_lossy().into_owned();
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("parent answer"),
        Script::SuccessText("forked answer"),
    ]);
    let mut product = spawn(&root, provider.address);
    let address = await_ready(&root, &mut product);

    let parent_run = cli(
        &root,
        &["run", "--service", &service, "--jsonl", "parent", "turn"],
    );
    assert_eq!(
        parent_run.code,
        Some(0),
        "parent run failed: {}",
        parent_run.stderr
    );
    let parent = parent_run.receipt()["session"]
        .as_str()
        .unwrap_or_else(|| panic!("the parent run named no Session"))
        .to_owned();
    let parent_history = format!("/api/session/{parent}/message");
    let before = json(address, "GET", &parent_history, "")["data"].clone();

    let forked = cli(
        &root,
        &[
            "run",
            "--service",
            &service,
            "-s",
            &parent,
            "--fork",
            "--jsonl",
            "forked",
            "turn",
        ],
    );
    assert_eq!(forked.code, Some(0), "fork run failed: {}", forked.stderr);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "the forked prompt did not execute"
    );
    // The child is not top-level, so `--resume` would not find it; the run's
    // receipt names the Session it ran in.
    let child = forked.receipt()["session"]
        .as_str()
        .unwrap_or_else(|| panic!("the fork run named no Session"))
        .to_owned();
    assert_ne!(child, parent, "CCM118 the forked prompt ran in the parent");
    let info = json(address, "GET", &format!("/api/session/{child}"), "");
    assert_eq!(
        info["data"]["parentID"],
        parent.as_str(),
        "the child names no parent"
    );
    let child_rows =
        json(address, "GET", &format!("/api/session/{child}/message"), "")["data"].to_string();
    for text in [
        "parent turn",
        "parent answer",
        "forked turn",
        "forked answer",
    ] {
        assert!(
            child_rows.contains(text),
            "the child history lacks {text:?}: {child_rows}"
        );
    }
    assert_eq!(
        json(address, "GET", &parent_history, "")["data"],
        before,
        "CCM118 the fork run changed the parent's history"
    );

    product.stop_after_witness(&root);
    drop(product);
    drop(provider);
    process_fixture::cleanup(&root);
}

/// Runs the shipped CLI with the scripted provider's address in its environment,
/// which a product the CLI itself starts inherits.
fn with_provider(root: &Path, arguments: &[&str], provider: SocketAddr) -> std::process::Output {
    Command::new(super::cli_continuity::cli_binary())
        .env("STATEENGINE_AGENT_ID", "fixture-launcher-must-not-inherit")
        .env("SECODE_STATEENGINE_SEAT_LAUNCH_PIN", "fixture-launcher-pin")
        .env("SECODE_SESSION_ID", "fixture-launcher-session")
        .current_dir(root)
        .args(arguments)
        .env("GRAPT_LIVE_HEADLESS_LOOPBACK", format!("http://{provider}"))
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("CLI launch failed: {error}"))
}

/// Stops the product `service start` launched: it is not a fixture-owned child.
fn stop_started_service(pid: u64, root: &Path) {
    let pid = pid.to_string();
    #[cfg(unix)]
    let stopped = Command::new("kill").args(["-9", &pid]).status();
    #[cfg(windows)]
    let stopped = Command::new("taskkill").args(["/F", "/PID", &pid]).status();
    assert!(
        stopped.is_ok_and(|status| status.success()),
        "the started service {pid} could not be stopped"
    );
    let wait = PollDeadline::new();
    while wait.pending() {
        if cli(
            root,
            &["service", "status", "--service", &root.to_string_lossy()],
        )
        .stdout
            == "stopped\n"
        {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    for name in [
        "registration",
        "registration.elected",
        "registration.lock",
        "serve-ready.json",
    ] {
        match fs::remove_file(root.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("stale {name} cleanup failed: {error}"),
        }
    }
}
