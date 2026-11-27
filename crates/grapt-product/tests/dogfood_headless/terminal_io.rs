//! A real PTY runs through production permissions, tool capture and continuation.
use super::*;

#[cfg(target_os = "linux")]
#[test]
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
fn contained_ctrl_c_preserves_the_command_handler_and_exit_status(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    for tty in [false, true] {
        let base = root(if tty {
            "sandbox-ctrl-c-pty"
        } else {
            "sandbox-ctrl-c-pipes"
        });
        let root = base.join("service");
        prepare(&root, 200_000);
        let install = base.join("install");
        let home = base.join("home");
        let config = base.join("global-config");
        let workspace = base.join("workspace");
        for directory in [&install, &home, &config, &workspace] {
            fs::create_dir(directory)?;
        }
        let binary = install.join("grapt-server");
        fs::copy(shipped_cli::product_binary(), &binary)?;
        fs::write(
            config.join("grapt.json"),
            serde_json::to_vec(&serde_json::json!({
                "sandbox":{"enabled":true,"network":false,"writable_roots":[workspace]}
            }))?,
        )?;
        let provider = ProviderFixture::start(vec![
            Script::ToolInput {
                call: "call_contained_start",
                tool: "exec_command",
                arguments: serde_json::json!({"cmd":"trap 'printf SIGINT_CAUGHT; exit 23' INT; printf READY; sleep 60",
                    "tty":tty,"yield_time_ms":1000,"timeout_ms":10_000}),
            },
            Script::Success,
            Script::TerminalInput {
                call: "call_contained_interrupt",
                source_call: "call_contained_start",
                chars: "\u{3}",
            },
            Script::Success,
        ]);
        let mut command = grapt_service::launcher_environment::command(binary);
        command
            .args(["serve", "--service"])
            .arg(&root)
            .args(["--port", "0"])
            .env("GRAPT_TEST_CATALOG_ROOT", &base)
            .env("GRAPT_TEST_HOME", &home)
            .env("GRAPT_CONFIG_DIR", &config)
            .env(
                "GRAPT_LIVE_HEADLESS_LOOPBACK",
                format!("http://{}", provider.address),
            );
        grapt_service::launcher_environment::scrub(&mut command);
        let mut child = process_fixture::spawn_captured(&root, &mut command);
        let address = await_ready(&root, &mut child);
        let session = json(
            address,
            "POST",
            "/api/session",
            &serde_json::json!({"location":{"directory":workspace}}).to_string(),
        )["data"]["id"]
            .as_str()
            .ok_or("Session identity absent")?
            .to_owned();
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"text":"Run and interrupt the contained command"}"#,
        );
        wait_calls(&provider.calls, 2);
        permission_residuals::wait_session_idle(address, &session);
        // Check native startup before the fixture needs an operation to interrupt.
        // A refused launch must name its receipt, not become a provider parse panic.
        {
            let bodies = locked(&provider.bodies);
            let continued: serde_json::Value =
                serde_json::from_str(bodies.last().ok_or("startup continuation absent")?)?;
            let raw = continued["input"]
                .as_array()
                .ok_or("startup continuation input absent")?
                .iter()
                .find(|item| {
                    item["type"] == "function_call_output"
                        && item["call_id"] == "call_contained_start"
                })
                .and_then(|item| item["output"].as_str())
                .ok_or("contained startup outcome absent")?;
            let initial: serde_json::Value = serde_json::from_str(raw)
                .map_err(|_| format!("Contained command refused before interruption: {raw}"))?;
            assert!(
                initial["ran"] == true
                    && initial["enforcement"]["kind"] == "platform_sandbox"
                    && initial["enforcement"]["backend"] == "bwrap",
                "Contained command refused before interruption: {raw}"
            );
        }
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"text":"Interrupt the same contained command"}"#,
        );
        wait_calls(&provider.calls, 4);
        permission_residuals::wait_session_idle(address, &session);
        let bodies = locked(&provider.bodies);
        let continued: serde_json::Value =
            serde_json::from_str(bodies.last().ok_or("continuation absent")?)?;
        let raw = continued["input"]
            .as_array()
            .ok_or("continuation input absent")?
            .iter()
            .find(|item| {
                item["type"] == "function_call_output"
                    && item["call_id"] == "call_contained_interrupt"
            })
            .and_then(|item| item["output"].as_str())
            .ok_or("interrupt outcome absent")?;
        let outcome: serde_json::Value = serde_json::from_str(raw)?;
        assert_eq!(outcome["exit_code"], 23, "{outcome}");
        assert!(
            outcome["output"]
                .as_str()
                .is_some_and(|text| text.contains("SIGINT_CAUGHT")),
            "{outcome}"
        );
        assert_eq!(outcome["enforcement"]["backend"], "bwrap", "{outcome}");
        assert_eq!(outcome["cleanup"], "complete", "{outcome}");
        drop(bodies);
        child.stop_after_witness(&root);
        drop(child);
        process_fixture::cleanup(&base);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
fn exact_write_consent_retries_inside_containment_and_revokes_each_scope(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let next_ask = |address, session: &str| -> Result<serde_json::Value, String> {
        let mut ask = None;
        let observed = permission_residuals::wait_until(
            Duration::from_secs(20),
            Duration::from_millis(20),
            || {
                let listed = json(
                    address,
                    "GET",
                    &format!("/api/session/{session}/permission"),
                    "",
                );
                ask = listed["data"]
                    .as_array()
                    .and_then(|rows| rows.first())
                    .cloned();
                ask.is_some()
            },
        );
        if observed.is_none() {
            return Err(
                "A generic bypass silently expanded containment or the grant ask was lost".into(),
            );
        }
        ask.ok_or_else(|| "Sandbox consent request absent".into())
    };
    for (reply, scope) in [
        ("once", "run"),
        ("session", "session"),
        ("always", "project"),
    ] {
        let base = root(&format!("sandbox-grant-{reply}"));
        let service = base.join("service");
        prepare(&service, 200_000);
        let install = base.join("install");
        let home = base.join("home");
        let config = base.join("config");
        let workspace = base.join("workspace");
        let temporary = base.join("temporary");
        for path in [&install, &home, &config, &workspace, &temporary] {
            fs::create_dir(path)?;
        }
        let target = base.join("approved[*]-file");
        let sibling = base.join("approved[probe]-file");
        fs::write(&target, "original")?;
        fs::write(&sibling, "sibling-original")?;
        fs::write(
            config.join("grapt.json"),
            br#"{"sandbox":{"enabled":true,"network":false}}"#,
        )?;
        let mut settings = fs::read_to_string(service.join("service.conf"))?;
        settings.push_str("permission_preset=bypass\n");
        fs::write(service.join("service.conf"), settings)?;
        let quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
        let provider = ProviderFixture::start(vec![
            Script::ToolInput {
                call: "call_withdrawn_ask",
                tool: "request_sandbox_access",
                arguments: serde_json::json!({"path":target,"cmd":format!("printf UNAPPROVED > {}", quote(&target))}),
            },
            Script::Success,
            Script::ToolInput {
                call: "call_exact_retry",
                tool: "request_sandbox_access",
                arguments: serde_json::json!({"path":target,"host":null,"cmd":format!("printf ALLOWED > {}; if printf ESCAPE > {}; then exit 91; fi; printf NARROW_RETRY_READY; sleep 60", quote(&target), quote(&sibling)),"yield_time_ms":250,"timeout_ms":60_000}),
            },
            Script::Success,
            Script::TerminalInput {
                call: "call_revoked_poll",
                source_call: "call_exact_retry",
                chars: "",
            },
            Script::Success,
        ]);
        let binary = install.join("grapt-server");
        fs::copy(shipped_cli::product_binary(), &binary)?;
        let mut command = grapt_service::launcher_environment::command(binary);
        command
            .args(["serve", "--service"])
            .arg(&service)
            .args(["--port", "0"])
            .env("GRAPT_TEST_CATALOG_ROOT", &base)
            .env("GRAPT_TEST_HOME", &home)
            .env("GRAPT_CONFIG_DIR", &config)
            .env("TMPDIR", &temporary)
            .env(
                "GRAPT_LIVE_HEADLESS_LOOPBACK",
                format!("http://{}", provider.address),
            );
        grapt_service::launcher_environment::scrub(&mut command);
        let mut child = process_fixture::spawn_captured(&service, &mut command);
        let address = await_ready(&service, &mut child);
        let session = json(
            address,
            "POST",
            "/api/session",
            &serde_json::json!({"location":{"directory":workspace}}).to_string(),
        )["data"]["id"]
            .as_str()
            .ok_or("Session identity absent")?
            .to_owned();
        let _approver = approval::Attachment::open(address, &session);
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"text":"Request access, then withdraw it while approval is pending"}"#,
        );
        let old = next_ask(address, &session)?;
        let list_path = format!("/api/session/{session}/sandbox-grants");
        let withdrawn = json(
            address,
            "POST",
            &format!("{list_path}/revoke"),
            &serde_json::json!({"scope":"session","resource":target}).to_string(),
        );
        assert_eq!(withdrawn["state"], "complete", "{withdrawn}");
        wait_calls(&provider.calls, 2);
        permission_residuals::wait_session_idle(address, &session);
        let permission = old["id"].as_str().ok_or("Old permission identity absent")?;
        let (late_status, _) = request(
            address,
            "POST",
            &format!("/api/session/{session}/permission/{permission}/reply"),
            r#"{"reply":"always"}"#,
        );
        assert_eq!(
            late_status, 409,
            "A withdrawn pending ask must refuse a conflicting late approval"
        );
        assert_eq!(fs::read_to_string(&target)?, "original");
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"text":"Request fresh consent for the exact contained retry"}"#,
        );
        let ask = next_ask(address, &session)?;
        assert_eq!(
            fs::read_to_string(&target)?,
            "original",
            "Effect ran before consent"
        );
        assert_eq!(
            ask["save"],
            serde_json::json!([target]),
            "Remembered consent widened beyond the target"
        );
        let permission = ask["id"].as_str().ok_or("Permission identity absent")?;
        let (status, _) = request(
            address,
            "POST",
            &format!("/api/session/{session}/permission/{permission}/reply"),
            &serde_json::json!({"reply":reply}).to_string(),
        );
        assert_eq!(status, 204);
        wait_calls(&provider.calls, 4);
        permission_residuals::wait_session_idle(address, &session);
        assert_eq!(fs::read_to_string(&target)?, "ALLOWED");
        assert_eq!(fs::read_to_string(&sibling)?, "sibling-original");
        let inventory = json(address, "GET", &list_path, "");
        let grant = inventory["data"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["scope"] == scope))
            .ok_or("Grant scope absent")?;
        assert_eq!(grant["resource"], target.to_string_lossy().as_ref());
        let revoked = json(
            address,
            "POST",
            &format!("{list_path}/revoke"),
            &grant.to_string(),
        );
        assert_eq!(revoked["state"], "complete", "{revoked}");
        assert!(json(address, "GET", &list_path, "")["data"]
            .as_array()
            .is_some_and(Vec::is_empty));
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"text":"Poll the existing command after revocation"}"#,
        );
        wait_calls(&provider.calls, 6);
        permission_residuals::wait_session_idle(address, &session);
        let bodies = locked(&provider.bodies);
        let continued: serde_json::Value =
            serde_json::from_str(bodies.last().ok_or("Poll continuation absent")?)?;
        let output = continued["input"]
            .as_array()
            .ok_or("Poll input absent")?
            .iter()
            .find(|row| {
                row["type"] == "function_call_output" && row["call_id"] == "call_revoked_poll"
            })
            .and_then(|row| row["output"].as_str())
            .ok_or("Poll output absent")?;
        let output: serde_json::Value = serde_json::from_str(output)?;
        assert_eq!(output["status"], "cancelled", "{output}");
        assert_eq!(output["cleanup"], "complete", "{output}");
        assert_eq!(output["enforcement"]["backend"], "bwrap", "{output}");
        assert_eq!(fs::read_to_string(&sibling)?, "sibling-original");
        drop(bodies);
        child.stop_after_witness(&service);
        drop(child);
        process_fixture::cleanup(&base);
    }
    Ok(())
}

#[test]
fn pty_input_output_and_restart_refusal_cross_the_production_tool_path(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("terminal-io");
    prepare(&root, 200_000);
    #[cfg(windows)]
    let command = "set /p reply=ready^> & set reply";
    #[cfg(not(windows))]
    let command = "test -t 0 && test -t 1 || exit 17; printf 'ready> '; IFS= read -r word; printf '\\nreply=%s\\n' \"$word\"";
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_terminal_start",
            tool: "exec_command",
            arguments: serde_json::json!({"cmd":command,"tty":true,"yield_time_ms":250,"timeout_ms":30_000}),
        },
        Script::TerminalInput {
            call: "call_terminal_input",
            source_call: "call_terminal_start",
            chars: "seat\n",
        },
        Script::Success,
        Script::TerminalInput {
            call: "call_terminal_reopen",
            source_call: "call_terminal_start",
            chars: "",
        },
        Script::Success,
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"run the interactive command"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies).clone();
    let continued: serde_json::Value = serde_json::from_str(&bodies[2])?;
    let result = continued["input"]
        .as_array()
        .ok_or("continuation input absent")?
        .iter()
        .find(|item| {
            item["type"] == "function_call_output" && item["call_id"] == "call_terminal_input"
        })
        .and_then(|item| item["output"].as_str())
        .ok_or("terminal input result absent")?;
    let result: serde_json::Value = serde_json::from_str(result)?;
    assert_eq!(result["exit_code"], 0);
    assert_eq!(result["done"], true);
    assert!(result["output"]
        .as_str()
        .is_some_and(|text| text.contains("reply=seat")));
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    )
    .to_string();
    assert!(history.contains("reply=seat"), "{history}");
    child.stop_after_witness(&root);
    drop(child);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"poll the old handle after restart"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 5);
    let bodies = locked(&provider.bodies).clone();
    let refused: serde_json::Value = serde_json::from_str(&bodies[4])?;
    let outcome = refused["input"]
        .as_array()
        .ok_or("refusal input absent")?
        .iter()
        .find(|item| {
            item["type"] == "function_call_output" && item["call_id"] == "call_terminal_reopen"
        })
        .and_then(|item| item["output"].as_str())
        .ok_or("reopened terminal outcome absent")?;
    assert!(
        outcome.contains("terminal exited with status 0"),
        "{outcome}"
    );
    assert!(outcome.contains("no input was delivered"), "{outcome}");
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}
