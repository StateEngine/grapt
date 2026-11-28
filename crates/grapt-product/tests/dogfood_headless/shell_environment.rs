//! Explicit native-tool journey: real model tool dispatch, approval and shells.
use super::*;

#[test]
fn a_launcher_identity_never_reaches_embedded_shell_pipe_or_pty(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("launcher-identity-shell");
    prepare(&root, 120_000);
    let profile_path = root.join("grapt.jsonc");
    let mut profile: serde_json::Value = serde_json::from_slice(&fs::read(&profile_path)?)?;
    profile["shell_environment_policy"] = serde_json::json!({"include_only":["*"],"set":{
        "STATEENGINE_AGENT_ID":"identity-sentinel",
        "SECODE_STATEENGINE_SEAT_LAUNCH_PIN":"identity-sentinel",
        "SECODE_SESSION_ID":"identity-sentinel"}});
    fs::write(&profile_path, serde_json::to_vec(&profile)?)?;
    let command = if cfg!(windows) {
        "if defined STATEENGINE_AGENT_ID (exit /b 9) else (if defined SECODE_STATEENGINE_SEAT_LAUNCH_PIN (exit /b 9) else (if not defined SECODE_SESSION_ID (exit /b 9) else (if not \"%SC_ENV_INHERIT_CONTROL%\"==\"retained\" (exit /b 8) else (echo SC_ENV_CLEAN))))"
    } else {
        "test -z \"${STATEENGINE_AGENT_ID+x}${SECODE_STATEENGINE_SEAT_LAUNCH_PIN+x}\" && test \"$SC_ENV_INHERIT_CONTROL\" = retained && printf SC_ENV_CLEAN"
    };
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_identity_shell",
            tool: "shell",
            arguments: serde_json::json!({"command":command,"timeout_ms":30000}),
        },
        Script::ToolInput {
            call: "call_identity_pipe",
            tool: "exec_command",
            arguments: serde_json::json!({"cmd":command,"tty":false,"yield_time_ms":30000,"timeout_ms":30000}),
        },
        Script::ToolInput {
            call: "call_identity_pty",
            tool: "exec_command",
            arguments: serde_json::json!({"cmd":command,"tty":true,"yield_time_ms":30000,"timeout_ms":30000}),
        },
        Script::Success,
    ]);
    let output = Command::new(shipped_cli::cli_binary())
        .current_dir(&root)
        .args(["run", "--embedded", "--service"])
        .arg(&root)
        .arg("--product")
        .arg(process_fixture::installed_product_binary())
        .args([
            "--new",
            "--auto",
            "--prompt",
            "check the disposable child environment",
            "--jsonl",
        ])
        .env("STATEENGINE_AGENT_ID", "identity-sentinel")
        .env("SECODE_STATEENGINE_SEAT_LAUNCH_PIN", "identity-sentinel")
        .env("SECODE_SESSION_ID", "identity-sentinel")
        .env("SC_ENV_INHERIT_CONTROL", "retained")
        .env("GRAPT_TEST_HOME", root.join("home"))
        .env("GRAPT_CONFIG_DIR", root.join("global-config"))
        .env("GRAPT_TEST_CATALOG_ROOT", &root)
        .env(
            "GRAPT_LIVE_HEADLESS_LOOPBACK",
            format!("http://{}", provider.address),
        )
        .stdin(Stdio::null())
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bodies = locked(&provider.bodies).clone();
    assert_eq!(bodies.len(), 4);
    for (index, call) in [
        "call_identity_shell",
        "call_identity_pipe",
        "call_identity_pty",
    ]
    .iter()
    .enumerate()
    {
        let request: serde_json::Value = serde_json::from_str(&bodies[index + 1])?;
        let output = request["input"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|item| item["type"] == "function_call_output" && item["call_id"] == *call)
            .and_then(|item| item["output"].as_str())
            .ok_or("child tool output missing")?;
        assert!(output.contains("SC_ENV_CLEAN"), "{call}: {output}");
    }
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
#[ignore = "requires an explicit native repository with a reachable origin; run with --ignored"]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn native_shell_environment_resolves_cargo_and_remote_git() -> Result<(), Box<dyn std::error::Error>>
{
    let _journey = process_journey_custody();
    let repository = std::env::var_os("GRAPT_NATIVE_GIT_REPOSITORY")
        .map(PathBuf::from)
        .ok_or("GRAPT_NATIVE_GIT_REPOSITORY is required")?;
    let repository = fs::canonicalize(repository)?;
    let root = root("native-shell-environment");
    prepare(&root, 240_000);
    let profile_path = root.join("grapt.jsonc");
    let original_profile = fs::read(&profile_path)?;
    let mut profile: serde_json::Value = serde_json::from_slice(&original_profile)?;
    profile["provider"]["credential"] = "env:SC_NATIVE_AUTH".into();
    fs::write(&profile_path, serde_json::to_vec(&profile)?)?;
    let command = if cfg!(windows) {
        "if defined SC_NATIVE_AUTH (exit /b 9) else (if defined SC_NATIVE_API_KEY (exit /b 9) else (cargo --version && git ls-remote origin))"
    } else {
        "test -z \"${SC_NATIVE_AUTH+x}${SC_NATIVE_API_KEY+x}\" && cargo --version && git ls-remote origin"
    };
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_native_shell",
            tool: "shell",
            arguments: serde_json::json!({"command":command,"timeout_ms":120000}),
        },
        Script::ToolInput {
            call: "call_native_pipe",
            tool: "exec_command",
            arguments: serde_json::json!({"cmd":command,"tty":false,"yield_time_ms":30000,"timeout_ms":120000}),
        },
        Script::ToolInput {
            call: "call_native_pty",
            tool: "exec_command",
            arguments: serde_json::json!({"cmd":command,"tty":true,"yield_time_ms":30000,"timeout_ms":120000}),
        },
        Script::Success,
    ]);
    let mut child = process_fixture::spawn_controlled_with(
        &root,
        provider.address,
        &[
            ("SC_NATIVE_API_KEY", "secret-sentinel"),
            ("SC_NATIVE_AUTH", "credential-probe-sentinel"),
        ],
    );
    let address = await_ready(&root, &mut child);
    fs::write(&profile_path, original_profile)?;
    let session = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({"location":{"directory":repository}}).to_string(),
    )["data"]["id"]
        .as_str()
        .ok_or("native Session missing")?
        .to_owned();
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_native_environment","text":"check developer tools","delivery":"queue","resume":true}"#,
    );
    compaction_continuity::approve_batch(
        address,
        &session,
        &["call_native_shell", "call_native_pipe", "call_native_pty"],
    );
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies).clone();
    assert_eq!(
        bodies.len(),
        4,
        "each real tool must return to the provider"
    );
    for (index, call) in ["call_native_shell", "call_native_pipe", "call_native_pty"]
        .iter()
        .enumerate()
    {
        let request: serde_json::Value = serde_json::from_str(&bodies[index + 1])?;
        let output = request["input"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|item| item["type"] == "function_call_output" && item["call_id"] == *call)
            .and_then(|item| item["output"].as_str())
            .ok_or("native tool output missing")?;
        assert!(
            output.contains("cargo "),
            "{call} failed to execute cargo: {output}"
        );
        assert!(
            output.contains("refs/heads/"),
            "{call} failed to reach origin: {output}"
        );
        assert!(!output.contains("secret-sentinel"));
        if index == 0 {
            assert!(output.contains("Command exited with code 0."), "{output}");
        } else {
            let report: serde_json::Value = serde_json::from_str(output)?;
            assert_eq!(report["exit_code"], 0, "{call} did not settle successfully");
            assert_eq!(
                report["done"], true,
                "{call} exceeded its initial observation window"
            );
        }
        println!(
            "{call}: cargo resolved from parent PATH; git ls-remote origin returned refs; exit 0"
        );
    }
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}
