use super::*;

fn spawn_production(root: &Path) -> process_fixture::ManagedChild {
    let mut command =
        grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"));
    command
        .arg("serve")
        .arg("--service")
        .arg(root)
        .arg("--port")
        .arg("0");
    process_fixture::spawn_captured(root, &mut command)
}

/// Explicitly billable, operator-supplied canary. The service root must be a
/// disposable prepared root whose Basic credential is `alpha:zephyr`, whose
/// `provider_mode` is `production`, and whose provider credential is real.
/// The test never reads or prints that provider credential.
#[test]
#[ignore = "explicit operator-approved billable real-model canary"]
fn billable_real_model_prompt_reaches_durable_headless_history() {
    let _journey = process_journey_custody();
    let root = std::env::var_os("GRAPT_REAL_MODEL_SERVICE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("GRAPT_REAL_MODEL_SERVICE_ROOT is absent"));
    assert!(root.is_absolute(), "billable canary root must be absolute");
    assert!(
        root.join("provider.credential").is_file(),
        "provider credential is absent"
    );
    let mut child = spawn_production(&root);
    let address = await_ready(&root, &mut child);
    let created = json(address, "POST", "/api/session", "{}");
    let session = created["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("created Session had no id"));
    let prompt_path = format!("/api/session/{session}/prompt");
    let prompt = json(
        address,
        "POST",
        &prompt_path,
        r#"{"id":"msg_billable_real_model_1","text":"Reply with one short sentence confirming this is the real-model headless canary.","delivery":"queue","resume":true}"#,
    );
    assert_eq!(prompt["data"]["id"], "msg_billable_real_model_1");
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    );
    let encoded = history.to_string();
    assert!(
        encoded.contains("assistant"),
        "real-model assistant history was absent"
    );
    assert!(
        !encoded.contains("deterministic answer for:"),
        "production mode resurrected the deterministic provider path"
    );
    child
        .kill()
        .unwrap_or_else(|error| panic!("billable canary shutdown failed: {error}"));
    let _ = child
        .wait()
        .unwrap_or_else(|error| panic!("billable canary wait failed: {error}"));
}
