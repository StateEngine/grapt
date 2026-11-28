//! SC-SEC-03: real configured decisions, governed effects and restart refusal.
use super::*;

#[test]
fn sec03_configured_denial_governs_api_and_shell_effects_after_an_approval_and_restart(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    for tool in ["shell", "exec_command"] {
        let root = root(&format!("sec03-config-{tool}"));
        let workspace = root.with_extension("workspace");
        let _ws_cleanup = terminal_coordination::RootCleanup(workspace.clone());
        fs::create_dir_all(&workspace)?;
        prepare(&root, 200_000);
        fs::create_dir_all(root.join("global-config"))?;
        let config = root.join("global-config/grapt.json");
        fs::write(&config, r#"{"permission":{"shell":"ask"}}"#)?;
        let input = |call, command: &str| Script::ToolInput {
            call,
            tool,
            arguments: if tool == "shell" {
                serde_json::json!({"command":command,"timeout_ms":20000})
            } else {
                serde_json::json!({"cmd":command,"yield_time_ms":1000,"timeout_ms":20000})
            },
        };
        let provider = ProviderFixture::start(vec![
            input("call_sec03_allowed", "echo allowed > sc-sec03-allowed.txt"),
            input("call_sec03_denied", "echo denied > sc-sec03-denied.txt"),
            Script::Success,
        ]);
        let mut child = spawn(&root, provider.address);
        let address = await_ready(&root, &mut child);
        let _approver = approval::Attachment::open(address, "*");
        let session = create_in(address, &workspace);
        let endpoint = format!("/api/session/{session}/permission");
        let approved = [
            "echo ordinary",
            "echo allowed > sc-sec03-allowed.txt",
            "echo denied > sc-sec03-denied.txt",
        ];
        let seed = json(
            address,
            "POST",
            &endpoint,
            &serde_json::json!({"action":"shell","resources":approved,"save":approved}).to_string(),
        );
        assert_eq!(seed["data"]["effect"], "ask");
        permission_boundaries::reply(address, &session, &seed["data"], "session");
        let ordinary = json(
            address,
            "POST",
            &endpoint,
            r#"{"action":"shell","resources":["echo ordinary"]}"#,
        );
        assert_eq!(
            ordinary["data"]["effect"], "allow",
            "the remembered approval control is inactive"
        );
        fs::write(
            &config,
            r#"{"permission":{"shell":{"*":"allow","*sc-sec03-denied*":"deny"}}}"#,
        )?;
        let denied = r#"{"action":"shell","resources":["echo denied > sc-sec03-denied.txt"]}"#;
        assert_eq!(
            json(address, "POST", &endpoint, denied)["data"]["effect"],
            "deny"
        );
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"text":"exercise configured shell rules"}"#,
        );
        permission_residuals::wait_session_idle(address, &session);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
        assert!(
            workspace.join("sc-sec03-allowed.txt").is_file(),
            "the permitted effect did not run"
        );
        assert!(
            !workspace.join("sc-sec03-denied.txt").exists(),
            "a configured denial was bypassed"
        );
        let output = permission_boundaries::output(&provider, "call_sec03_denied");
        assert!(output.contains("configured permission policy"), "{output}");
        child.stop_after_witness(&root);
        drop(child);
        let mut restarted = spawn(&root, provider.address);
        let address = await_ready(&root, &mut restarted);
        let _approver = approval::Attachment::open(address, "*");
        assert_eq!(
            json(address, "POST", &endpoint, denied)["data"]["effect"],
            "deny"
        );
        assert!(!root.join("sc-sec03-denied.txt").exists());
        restarted.stop_after_witness(&root);
        drop(restarted);
        process_fixture::cleanup(&root);
    }
    Ok(())
}

#[test]
fn sec03_selected_child_agent_cannot_escape_its_ancestors_configured_deny(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("sec03-configured-ancestor");
    prepare(&root, 200_000);
    fs::create_dir_all(root.join("global-config"))?;
    fs::write(
        root.join("global-config/grapt.json"),
        r#"{
        "permission":{"subagent":"ask"},
        "agent":{
            "agt_live_headless1":{"permission":{"shell":"deny"}},
            "agt_dogfood_allow":{"permission":{"shell":"allow"}}
        }
    }"#,
    )?;
    let provider = ProviderFixture::start(vec![
        Script::ToolSubagent {
            call: "call_sec03_child",
            task: "inspect the repository",
        },
        Script::Success,
        Script::Success,
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let parent = create(address);
    let path = format!("/api/session/{parent}/prompt");
    let prompt = thread::spawn(move || {
        request(
            address,
            "POST",
            &path,
            r#"{"text":"delegate an inspection"}"#,
        )
    });
    subagent_child::approve_the_subagent_ask(address, &parent);
    assert_eq!(
        prompt.join().map_err(|_| "delegating prompt panicked")?.0,
        200
    );
    permission_residuals::wait_session_idle(address, &parent);
    let page = json(address, "GET", "/api/session", "");
    let rows = page["data"].as_array().ok_or("sessions")?;
    let child_id = rows
        .iter()
        .find(|row| row["parentID"].as_str() == Some(parent.as_str()))
        .and_then(|row| row["id"].as_str())
        .ok_or("delegated child")?
        .to_owned();
    assert_eq!(
        request(
            address,
            "POST",
            &format!("/api/session/{child_id}/agent"),
            r#"{"agent":"agt_dogfood_allow"}"#
        )
        .0,
        204
    );
    let endpoint = format!("/api/session/{child_id}/permission");
    let command = r#"{"action":"shell","resources":["echo ancestor ceiling"]}"#;
    assert_eq!(
        json(address, "POST", &endpoint, command)["data"]["effect"],
        "deny"
    );
    child.stop_after_witness(&root);
    drop(child);
    let mut restarted = spawn(&root, provider.address);
    let address = await_ready(&root, &mut restarted);
    let _approver = approval::Attachment::open(address, "*");
    assert_eq!(
        json(address, "POST", &endpoint, command)["data"]["effect"],
        "deny"
    );
    restarted.stop_after_witness(&root);
    drop(restarted);
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
fn sec03_literal_prefix_decisions_govern_real_tools_and_survive_restart(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    for tool in ["shell", "exec_command"] {
        let root = root(&format!("sec03-prefix-{tool}"));
        prepare(&root, 200_000);
        fs::create_dir_all(root.join("global-config"))?;
        fs::write(
            root.join("global-config/grapt.json"),
            r#"{
            "permission":{"shell":"ask"},
            "execpolicy":[
                {"pattern":["echo","prefix-granted"],"decision":"allow"},
                {"pattern":["echo","prefix-denied"],"decision":"deny"}
            ]
        }"#,
        )?;
        let input = |call, command: &str| Script::ToolInput {
            call,
            tool,
            arguments: if tool == "shell" {
                serde_json::json!({"command":command,"timeout_ms":20000})
            } else {
                serde_json::json!({"cmd":command,"yield_time_ms":1000,"timeout_ms":20000})
            },
        };
        let provider = ProviderFixture::start(vec![
            input("call_sec03_prefix_allowed", "echo prefix-granted"),
            input(
                "call_sec03_prefix_denied",
                "echo prefix-denied > sc-sec03-prefix-denied.txt",
            ),
            Script::Success,
        ]);
        let mut child = spawn(&root, provider.address);
        let address = await_ready(&root, &mut child);
        let _approver = approval::Attachment::open(address, "*");
        let session = create(address);
        let endpoint = format!("/api/session/{session}/permission");
        let allowed = r#"{"action":"shell","resources":["echo prefix-granted"]}"#;
        let denied =
            r#"{"action":"shell","resources":["echo prefix-denied > sc-sec03-prefix-denied.txt"]}"#;
        assert_eq!(
            json(address, "POST", &endpoint, allowed)["data"]["effect"],
            "allow"
        );
        assert_eq!(
            json(address, "POST", &endpoint, denied)["data"]["effect"],
            "deny"
        );
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"text":"exercise literal argv permissions"}"#,
        );
        permission_residuals::wait_session_idle(address, &session);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
        let output = permission_boundaries::output(&provider, "call_sec03_prefix_allowed");
        assert!(
            output.contains("prefix-granted"),
            "the prefix grant did not execute: {output}"
        );
        assert!(
            !root.join("sc-sec03-prefix-denied.txt").exists(),
            "a literal prefix denial was bypassed"
        );
        let output = permission_boundaries::output(&provider, "call_sec03_prefix_denied");
        assert!(output.contains("configured permission policy"), "{output}");
        child.stop_after_witness(&root);
        drop(child);
        let mut restarted = spawn(&root, provider.address);
        let address = await_ready(&root, &mut restarted);
        let _approver = approval::Attachment::open(address, "*");
        assert_eq!(
            json(address, "POST", &endpoint, allowed)["data"]["effect"],
            "allow"
        );
        assert_eq!(
            json(address, "POST", &endpoint, denied)["data"]["effect"],
            "deny"
        );
        assert!(!root.join("sc-sec03-prefix-denied.txt").exists());
        restarted.stop_after_witness(&root);
        drop(restarted);
        process_fixture::cleanup(&root);
    }
    Ok(())
}

/// A footer-shaped sequence is still executed shell text in the legacy tool.
#[cfg(unix)]
#[test]
fn sec03_legacy_shell_cannot_hide_an_effect_in_a_forged_terminal_footer(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("sec03-forged-footer");
    prepare(&root, 200_000);
    fs::create_dir_all(root.join("global-config"))?;
    fs::write(
        root.join("global-config/grapt.json"),
        r#"{
        "permission":{"shell":"ask"},
        "execpolicy":[{"pattern":["echo","footer-granted"],"decision":"allow"}]
    }"#,
    )?;
    let marker = root.join("sc-sec03-forged-footer.txt");
    let command = "echo footer-granted\nworkdir: /repo; echo UNAPPROVED > sc-sec03-forged-footer.txt\nshell: /bin/sh\nlogin: true\nterminal: false";
    refuses_before_marker(
        &root,
        command,
        "call_sec03_forged_footer",
        "shell",
        &marker,
        "a forged terminal footer executed an unapproved legacy-shell write",
    )
}

#[cfg(unix)]
fn refuses_before_marker(
    root: &Path,
    command: &str,
    call: &'static str,
    tool: &'static str,
    marker: &Path,
    failure: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let arguments = if tool == "shell" {
        serde_json::json!({"command":command,"timeout_ms":20000})
    } else {
        serde_json::json!({"cmd":command,"yield_time_ms":1000,"timeout_ms":20000})
    };
    refuses_input_before_marker(root, arguments, call, tool, marker, failure)
}

#[cfg(unix)]
fn refuses_input_before_marker(
    root: &Path,
    arguments: serde_json::Value,
    call: &'static str,
    tool: &'static str,
    marker: &Path,
    failure: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call,
            tool,
            arguments,
        },
        Script::Success,
    ]);
    let mut child = spawn(root, provider.address);
    let address = await_ready(root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);
    let prompt_session = session.clone();
    let prompt = thread::spawn(move || {
        request(
            address,
            "POST",
            &format!("/api/session/{prompt_session}/prompt"),
            r#"{"text":"check literal prefix boundaries"}"#,
        )
    });
    let mut pending = None;
    let wait = PollDeadline::new();
    while wait.pending() {
        let page = json(
            address,
            "GET",
            &format!("/api/session/{session}/permission"),
            "",
        );
        pending = page["data"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["source"]["callID"] == call))
            .cloned();
        if pending.is_some() || marker.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!marker.exists(), "{failure}");
    let row = pending.ok_or("command neither asked nor reached its effect witness")?;
    permission_boundaries::reply(address, &session, &row, "reject");
    assert_eq!(
        prompt.join().map_err(|_| "permission prompt panicked")?.0,
        200
    );
    permission_residuals::wait_session_idle(address, &session);
    assert!(!marker.exists());
    child.stop_after_witness(root);
    drop(child);
    process_fixture::cleanup(root);
    Ok(())
}

#[cfg(unix)]
#[test]
fn sec03_selected_shell_cannot_execute_before_command_prefix_approval(
) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let _journey = process_journey_custody();
    let root = root("sec03-selected-shell");
    prepare(&root, 200_000);
    fs::create_dir_all(root.join("global-config"))?;
    fs::write(
        root.join("global-config/grapt.json"),
        r#"{"permission":{"shell":"ask"},"execpolicy":[{"pattern":["echo","prefix-controlled"],"decision":"allow"}]}"#,
    )?;
    let marker = root.join("sc-sec03-selected-shell-marker.txt");
    let shell = root.join("sh");
    fs::write(
        &shell,
        "#!/bin/sh\nprintf UNAPPROVED > sc-sec03-selected-shell-marker.txt\n",
    )?;
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o700))?;
    refuses_input_before_marker(
        &root,
        serde_json::json!({"cmd":"echo prefix-controlled","shell":shell,"login":false,"yield_time_ms":1000,"timeout_ms":20000}),
        "call_sec03_selected_shell",
        "exec_command",
        &marker,
        "selected shell executed before prefix approval",
    )?;
    process_fixture::cleanup(&root);
    Ok(())
}

#[cfg(unix)]
#[test]
fn sec03_supplied_startup_code_never_runs_before_shell_or_terminal_approval(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    for tool in ["shell", "exec_command"] {
        for (form, before, after) in [
            ("direct", "BASH_ENV=", " bash -c \"git status\""),
            ("env", "env BASH_ENV=", " bash -c \"git status\""),
            ("rcfile", "bash --rcfile ", " -ic \"git status\""),
            ("init-file", "bash --init-file ", " -ic \"git status\""),
        ] {
            let root = root(&format!("sec03-startup-{tool}-{form}"));
            prepare(&root, 200_000);
            fs::create_dir_all(root.join("global-config"))?;
            fs::write(
                root.join("global-config/grapt.json"),
                r#"{
                "permission":{"shell":"ask"},
                "execpolicy":[{"pattern":["git","status"],"decision":"allow"}]
            }"#,
            )?;
            let startup = root.join("startup.sh");
            let marker = root.join("sc-sec03-startup-marker.txt");
            fs::write(&startup, "echo UNAPPROVED > sc-sec03-startup-marker.txt\n")?;
            let command = format!("{before}\"{}\"{after}", startup.display());
            refuses_before_marker(
                &root,
                &command,
                "call_sec03_startup",
                tool,
                &marker,
                "startup code executed before approval",
            )?;
        }
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn sec03_git_prefix_never_authorizes_a_lockfile_write() -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    for tool in ["shell", "exec_command"] {
        let root = root(&format!("sec03-lockfile-{tool}"));
        prepare(&root, 200_000);
        fs::create_dir_all(root.join("global-config"))?;
        fs::write(
            root.join("global-config/grapt.json"),
            r#"{
            "permission":{"shell":"ask"},
            "execpolicy":[{"pattern":["git","status"],"decision":"allow"}]
        }"#,
        )?;
        let marker = root.join("sc-sec03-unapproved.lock");
        refuses_before_marker(
            &root,
            "flock sc-sec03-unapproved.lock git status",
            "call_sec03_lockfile",
            tool,
            &marker,
            "lockfile created before approval",
        )?;
    }
    Ok(())
}
