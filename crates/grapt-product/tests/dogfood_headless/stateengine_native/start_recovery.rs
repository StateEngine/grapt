//! One failed start is recoverable by the next ordinary Step in the same service.
use super::*;

#[test]
fn repository_owned_stateengine_is_named_as_blocked_without_starting_it() {
    let _journey = process_journey_custody();
    let root = root("native-blocked-configuration");
    prepare(&root, 90_000);
    configure(&root, "ready");
    let initialized = Command::new("git")
        .args(["init", "--quiet"])
        .arg(&root)
        .output()
        .unwrap_or_else(|error| panic!("fixture repository: {error}"));
    assert!(
        initialized.status.success(),
        "fixture Git initialization failed"
    );
    let provider = ProviderFixture::start(Vec::new());
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create_native(address, &root);
    bind(address, &session, true);
    let run = run_native(&root, &session);
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50"),
        "",
    );
    let catalog = json(address, "GET", "/api/mcp", "");
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    assert_ne!(run.code, Some(0));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let history = history.to_string();
    assert!(
        history.contains("repository-owned"),
        "blocked configured connection was reported missing: {history}"
    );
    assert!(!history.contains("no StateEngine connection is configured"));
    assert!(
        catalog["data"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["name"] == "stateengine"
                && row["status"]["status"] == "failed"
                && row["status"]["error"]
                    .as_str()
                    .is_some_and(|text| text.contains("project config")))),
        "blocked declaration disappeared from /mcps: {catalog}"
    );
}

#[test]
fn seat04_failed_start_has_real_stderr_and_next_step_recovers_without_service_restart() {
    use grapt_session::execution::{ContinuityFailure, StateEngineServerFailure};
    let _journey = process_journey_custody();
    let root = root("seat04-native-start-recovery");
    prepare(&root, 90_000);
    configure(&root, "ready");
    let config_path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(
        &fs::read(&config_path).unwrap_or_else(|error| panic!("fixture config: {error}")),
    )
    .unwrap_or_else(|error| panic!("fixture config JSON: {error}"));
    let command = config["mcp"]["stateengine"]["command"]
        .as_array_mut()
        .unwrap_or_else(|| panic!("fixture local command is absent"));
    command.extend([
        serde_json::json!("--fail-first-file"),
        serde_json::json!(root.join("door-starts")),
    ]);
    config["mcp"]["stateengine"]["environment"] =
        serde_json::json!({"SEAT04_FIXTURE_SECRET":"fixture-private-value"});
    fs::write(&config_path, config.to_string())
        .unwrap_or_else(|error| panic!("fixture config write: {error}"));
    let provider =
        ProviderFixture::start(vec![Script::SuccessText("Recovered in the same service.")]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let service_pid = child.id();
    let _approver = approval::Attachment::open(address, "*");
    let session = create_native(address, &root);
    bind(address, &session, true);
    let first = run_native(&root, &session);
    assert_ne!(
        first.code,
        Some(0),
        "first failed start reached the provider"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let failed = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50"),
        "",
    );
    let reason =
        ContinuityFailure::ServerUnavailable(StateEngineServerFailure::Disconnected).message();
    assert!(
        failed.to_string().contains(&reason),
        "missing distinct unavailable reason: {failed}"
    );
    let log = fs::read_to_string(root.join("product-stderr-0.log"))
        .unwrap_or_else(|error| panic!("product stderr evidence: {error}"));
    assert!(
        log.contains("first startup exited"),
        "lost startup stderr: {log}"
    );
    assert!(log.contains("[private]"));
    assert!(!log.contains("fixture-private-value"));
    assert!(
        !failed.to_string().contains("first startup exited"),
        "raw stderr entered durable history"
    );
    let second = run_native(&root, &session);
    assert_eq!(
        second.code,
        Some(0),
        "next ordinary Step did not recover: {}",
        second.stderr
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(child.id(), service_pid);
    assert_eq!(
        fs::read_to_string(root.join("door-starts"))
            .unwrap_or_else(|error| panic!("spawn count: {error}"))
            .trim(),
        "2"
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}
