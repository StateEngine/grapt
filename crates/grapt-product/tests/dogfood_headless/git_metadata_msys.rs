use super::*;

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn session_msys_consent_cannot_bypass_git_protection_in_real_tools(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let base = root("git-msys-consent");
    let service = base.join("service");
    prepare(&service, 60_000);
    let workspace = base.join("workspace");
    fs::create_dir(&workspace)?;
    let mut git = Command::new("git");
    git.args(["init", "--quiet"]).arg(&workspace);
    let (code, _, error) = project_list::bounded(git, Duration::from_secs(30));
    assert_eq!(code, Some(0), "fixture repository initialization: {error}");
    let before = fs::read(workspace.join(".git/HEAD"))?;
    fs::write(
        workspace.join("grapt.jsonc"),
        r#"{"sandbox":{"enabled":false,"git_metadata_read_only":true},"permission":{"*":"allow"}}"#,
    )?;
    let source = if cfg!(windows) {
        "echo WORKSPACE_WRITE>ordinary.txt & echo ESCAPE>.git\\HEAD & echo COMMAND_FINISHED"
    } else {
        "printf WORKSPACE_WRITE > ordinary.txt; printf ESCAPE > .git/HEAD; printf COMMAND_FINISHED"
    };
    let install = base.join("install");
    let home = base.join("home");
    let config = base.join("config");
    for path in [&install, &home, &config] {
        fs::create_dir(path)?;
    }
    // Cargo's hard-linked artifact is not an installed executable identity.
    let binary = install.join(if cfg!(windows) {
        "grapt-server.exe"
    } else {
        "grapt-server"
    });
    fs::copy(shipped_cli::product_binary(), &binary)?;
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_msys_git_shell",
            tool: "shell",
            arguments: serde_json::json!({"command":source}),
        },
        Script::ToolInput {
            call: "call_msys_git_pipe",
            tool: "exec_command",
            arguments: serde_json::json!({"cmd":source,"tty":false,"yield_time_ms":10000}),
        },
        Script::ToolInput {
            call: "call_msys_git_pty",
            tool: "exec_command",
            arguments: serde_json::json!({"cmd":source,"tty":true,"yield_time_ms":10000}),
        },
        Script::Success,
    ]);
    let mut command = grapt_service::launcher_environment::command(binary);
    command
        .args(["serve-terminal", "--service"])
        .arg(&service)
        .args(["--port", "0"])
        .stdin(Stdio::piped())
        .env("GRAPT_TEST_CATALOG_ROOT", &base)
        .env("GRAPT_TEST_HOME", &home)
        .env("GRAPT_CONFIG_DIR", &config)
        .env(
            "GRAPT_LIVE_HEADLESS_LOOPBACK",
            format!("http://{}", provider.address),
        );
    grapt_service::launcher_environment::scrub(&mut command);
    let mut child = process_fixture::spawn_captured(&service, &mut command);
    let address = await_ready(&service, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({"location":{"directory":workspace}}).to_string(),
    )["data"]["id"]
        .as_str()
        .ok_or("Session absent")?
        .to_owned();
    // Seed consent through the supported permission API before asking the
    // production dispatcher to prepare any of the protected command trees.
    let permission = json(
        address,
        "POST",
        &format!("/api/session/{session}/permission"),
        &serde_json::json!({
            "id":"per_prior_msys", "agent":"agt_live_headless1",
            "action":grapt_permission::SANDBOX_MSYS_ACTION,
            "resources":[grapt_permission::SANDBOX_MSYS_SESSION],
            "save":[grapt_permission::SANDBOX_MSYS_SESSION]
        })
        .to_string(),
    );
    assert_eq!(permission["data"]["effect"], "ask");
    let (status, _) = request(
        address,
        "POST",
        &format!("/api/session/{session}/permission/per_prior_msys/reply"),
        r#"{"reply":"session"}"#,
    );
    assert_eq!(status, 204);
    let reused = json(
        address,
        "POST",
        &format!("/api/session/{session}/permission"),
        &serde_json::json!({
            "id":"per_reused_msys", "agent":"agt_live_headless1",
            "action":grapt_permission::SANDBOX_MSYS_ACTION,
            "resources":[grapt_permission::SANDBOX_MSYS_SESSION]
        })
        .to_string(),
    );
    assert_eq!(
        reused["data"]["effect"], "allow",
        "Session grant was not retained"
    );
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"exercise protected commands with retained compatibility consent"}"#,
    );
    let wait = PollDeadline::new();
    while wait.pending() && provider.calls.load(Ordering::SeqCst) < 4 {
        let asked = json(
            address,
            "GET",
            &format!("/api/session/{session}/permission"),
            "",
        );
        for row in asked["data"].as_array().into_iter().flatten() {
            let action = row["action"].as_str().unwrap_or_default();
            assert_ne!(
                action,
                grapt_permission::SANDBOX_MSYS_ACTION,
                "retained MSYS consent unexpectedly re-prompted: {row}"
            );
            assert_eq!(
                action, "shell",
                "unexpected non-shell permission prompt: {row}"
            );
            if let Some(id) = row["id"].as_str() {
                let path = format!("/api/session/{session}/permission/{id}/reply");
                assert_eq!(
                    request(address, "POST", &path, r#"{"reply":"once"}"#).0,
                    204
                );
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    wait_calls(&provider.calls, 4);
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies).clone();
    for (index, call) in [
        "call_msys_git_shell",
        "call_msys_git_pipe",
        "call_msys_git_pty",
    ]
    .into_iter()
    .enumerate()
    {
        let body: serde_json::Value =
            serde_json::from_str(bodies.get(index + 1).ok_or("continuation absent")?)?;
        let raw = body["input"]
            .as_array()
            .ok_or("input absent")?
            .iter()
            .find(|item| item["type"] == "function_call_output" && item["call_id"] == call)
            .and_then(|item| item["output"].as_str())
            .ok_or("tool output absent")?;
        let receipt: serde_json::Value =
            serde_json::from_str(raw).map_err(|error| format!("{call}: {error}: {raw}"))?;
        assert_eq!(
            receipt["enforcement"]["kind"], "platform_sandbox",
            "{call}: {raw}"
        );
        assert!(
            receipt["output"]
                .as_str()
                .is_some_and(|output| output.contains("COMMAND_FINISHED")),
            "{call}: {raw}"
        );
    }
    assert!(fs::read_to_string(workspace.join("ordinary.txt"))?.contains("WORKSPACE_WRITE"));
    assert_eq!(fs::read(workspace.join(".git/HEAD"))?, before);
    child.stop_after_witness(&service);
    drop(child);
    process_fixture::cleanup(&base);
    Ok(())
}
