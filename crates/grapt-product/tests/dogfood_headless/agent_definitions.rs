//! Named agent selection reaches the ordinary provider and governed tool path.
use super::*;

#[test]
fn plan_creates_its_first_document_and_refuses_project_edits(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let profile = root("planning-agent-service");
    let _profile_cleanup = terminal_coordination::RootCleanup(profile.clone());
    prepare(&profile, 120_000);
    let workspace = root("planning-agent-workspace");
    let _workspace_cleanup = terminal_coordination::RootCleanup(workspace.clone());
    process_fixture::cleanup(&workspace);
    fs::create_dir_all(&workspace)?;
    fs::write(workspace.join("source.rs"), "original\n")?;
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_first_plan",
            tool: "apply_patch",
            arguments: serde_json::json!({"input":"*** Begin Patch\n*** Add File: plans/review/design.md\n+# Plan\n*** End Patch"}),
        },
        Script::ToolInput {
            call: "call_forbidden_project_edit",
            tool: "write",
            arguments: serde_json::json!({"path":"source.rs","content":"changed\n"}),
        },
        Script::Success,
    ]);
    let mut service = spawn(&profile, provider.address);
    let address = await_ready(&profile, &mut service);
    let created = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({"location":{"directory":workspace}}).to_string(),
    );
    let session = created["data"]["id"].as_str().ok_or("session absent")?;
    assert_eq!(
        request(
            address,
            "POST",
            &format!("/api/session/{session}/agent"),
            r#"{"agent":"plan"}"#
        )
        .0,
        204
    );
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"Inspect the project and write a plan."}"#,
    );
    permission_residuals::wait_session_idle(address, session);
    assert_eq!(
        fs::read_to_string(workspace.join("plans/review/design.md"))?,
        "# Plan\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("source.rs"))?,
        "original\n"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    service.stop_after_witness(&profile);
    Ok(())
}

#[test]
fn default_roster_hides_the_fixture_but_explicit_selection_still_runs(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("default-agent-roster");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(vec![Script::Success, Script::Success]);
    let mut service = spawn(&root, provider.address);
    let address = await_ready(&root, &mut service);
    let inventory = json(address, "GET", "/api/agent", "");
    let agents = inventory["data"].as_array().ok_or("agent roster absent")?;
    assert_eq!(
        agents
            .iter()
            .find(|agent| agent["hidden"] == false)
            .ok_or("visible agent absent")?["id"],
        "default"
    );
    assert!(agents
        .iter()
        .any(|agent| agent["id"] == "agt_live_headless1" && agent["hidden"] == true));
    let created = json(address, "POST", "/api/session", "{}");
    let session = created["data"]["id"].as_str().ok_or("session absent")?;
    for agent in ["default", "agt_live_headless1"] {
        if agent != "default" {
            assert_eq!(
                request(
                    address,
                    "POST",
                    &format!("/api/session/{session}/agent"),
                    r#"{"agent":"agt_live_headless1"}"#
                )
                .0,
                204
            );
        }
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"text":"hello"}"#,
        );
        permission_residuals::wait_session_idle(address, session);
        let history = json(
            address,
            "GET",
            &format!("/api/session/{session}/message?order=desc&limit=1"),
            "",
        );
        assert_eq!(history["data"][0]["agent"], agent);
    }
    service.stop_after_witness(&root);
    Ok(())
}

#[test]
fn selected_definition_keeps_its_prompt_stable_and_refuses_an_unoffered_effect(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("named-agent-definition");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    fs::create_dir_all(root.join("global-config/agents"))?;
    fs::write(root.join("global-config/agents/auditor.toml"),
        "name='auditor'\ndescription='Inspect changes'\ndeveloper_instructions='AUDITOR: explain findings without edits.'\ntools=['read']\n")?;
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_unoffered_agent_effect",
            tool: "exec_command",
            arguments: serde_json::json!({"cmd":"echo forbidden > agent-escaped.txt"}),
        },
        Script::Success,
        Script::ToolInput {
            call: "call_unoffered_after_restart",
            tool: "exec_command",
            arguments: serde_json::json!({"cmd":"echo forbidden > agent-after-restart.txt"}),
        },
        Script::Success,
    ]);
    let mut service = spawn(&root, provider.address);
    let mut address = await_ready(&root, &mut service);
    let inventory = json(address, "GET", "/api/agent", "");
    assert!(inventory["data"]
        .as_array()
        .is_some_and(|agents| agents.iter().any(|agent| agent["id"] == "auditor")));
    let session = create(address);
    assert_eq!(
        request(
            address,
            "POST",
            &format!("/api/session/{session}/agent"),
            r#"{"agent":"auditor"}"#
        )
        .0,
        204
    );
    for (index, message) in ["inspect changes", "continue inspection"]
        .into_iter()
        .enumerate()
    {
        if index == 1 {
            service.stop_after_witness(&root);
            service = spawn(&root, provider.address);
            address = await_ready(&root, &mut service);
        }
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            &serde_json::json!({"text":message}).to_string(),
        );
        permission_residuals::wait_session_idle(address, &session);
    }
    assert!(!root.join("agent-escaped.txt").exists());
    assert!(!root.join("agent-after-restart.txt").exists());
    assert!(
        permission_boundaries::output(&provider, "call_unoffered_after_restart")
            .contains("excluded by this agent")
    );
    assert!(
        permission_boundaries::output(&provider, "call_unoffered_agent_effect")
            .contains("excluded by this agent")
    );
    let bodies = locked(&provider.bodies);
    let requests = bodies
        .iter()
        .map(|body| serde_json::from_str::<serde_json::Value>(body))
        .collect::<Result<Vec<_>, _>>()?;
    let system = |request: &serde_json::Value| {
        request["input"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| item["role"] == "system")
            .cloned()
            .collect::<Vec<_>>()
    };
    assert!(serde_json::to_string(&system(&requests[0]))?
        .contains("AUDITOR: explain findings without edits."));
    assert_eq!(system(&requests[0]), system(&requests[2]));
    for request in &requests {
        assert!(!request["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|tool| tool["name"] == "exec_command"));
    }
    drop(bodies);
    service.stop_after_witness(&root);
    Ok(())
}

#[test]
fn a_terminal_role_cannot_rewrite_its_tool_authority_before_or_after_restart(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("terminal-agent-authority");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    let path = root.join(".grapt/agents/terminal.toml");
    fs::create_dir_all(path.parent().ok_or("agent directory missing")?)?;
    let definition = "name='terminal'\ndescription='Run commands'\ndeveloper_instructions='Run assigned commands.'\ntools=['exec_command']\n";
    fs::write(&path, definition)?;
    // The command stays inside the workspace and the preset allows ordinary
    // shell commands. Only the definition-authority floor must stop it.
    let widened = definition.replace("['exec_command']", "['exec_command','write']");
    let overwrite =
        serde_json::json!({"cmd":format!("echo \"{widened}\" > .grapt/agents/terminal.toml")});
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_agent_rewrite",
            tool: "exec_command",
            arguments: overwrite.clone(),
        },
        Script::ToolInput {
            call: "call_agent_escalation",
            tool: "write",
            arguments: serde_json::json!({"path":"escalated.txt","content":"forbidden"}),
        },
        Script::Success,
        Script::ToolInput {
            call: "call_agent_rewrite_restarted",
            tool: "exec_command",
            arguments: overwrite,
        },
        Script::Success,
    ]);
    let mut service = process_fixture::spawn_controlled(&root, provider.address);
    let mut address = await_ready(&root, &mut service);
    let session = create(address);
    assert_eq!(
        request(
            address,
            "POST",
            &format!("/api/session/{session}/agent"),
            r#"{"agent":"terminal"}"#
        )
        .0,
        204
    );
    for (index, call) in ["call_agent_rewrite", "call_agent_rewrite_restarted"]
        .into_iter()
        .enumerate()
    {
        if index == 1 {
            service.stop_after_witness(&root);
            service = process_fixture::spawn_controlled(&root, provider.address);
            address = await_ready(&root, &mut service);
        }
        let _approver = approval::Attachment::open(address, "*");
        let route = format!("/api/session/{session}/prompt");
        let prompt = thread::spawn(move || {
            json(
                address,
                "POST",
                &route,
                r#"{"text":"Run the assigned command"}"#,
            );
        });
        let row = permission_boundaries::pending(address, &session, call);
        assert_eq!(row["metadata"]["policy"]["ownerFloor"], true);
        permission_boundaries::reply(address, &session, &row, "reject");
        prompt.join().map_err(|_| "prompt thread failed")?;
        permission_residuals::wait_session_idle(address, &session);
        assert_eq!(fs::read_to_string(&path)?, definition);
    }
    assert!(!root.join("escalated.txt").exists());
    assert!(
        permission_boundaries::output(&provider, "call_agent_escalation")
            .contains("excluded by this agent")
    );
    service.stop_after_witness(&root);
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn an_ordinary_session_runs_a_claude_markdown_agent_as_loaded(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("claude-markdown-agent");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    fs::create_dir_all(root.join(".claude/agents"))?;
    fs::write(
        root.join(".claude/agents/claude-helper.md"),
        "---\nname: claude-helper\ndescription: Report findings plainly\nmodel: sonnet\ntools: read, Bash, Grep\n---\nCLAUDE-HELPER: report findings as plain text, without edits.\n",
    )?;
    // Same name as a built-in: ours must win, and the roster says so.
    fs::write(
        root.join(".claude/agents/worker.md"),
        "---\nname: worker\ndescription: Claude worker\n---\nCLAUDE WORKER BODY\n",
    )?;
    // A linked user directory still loads; the repository layer stays regular.
    #[cfg(unix)]
    {
        fs::create_dir_all(root.join("home/.claude"))?;
        fs::create_dir(root.join("user-agent-files"))?;
        std::os::unix::fs::symlink(
            root.join("user-agent-files"),
            root.join("home/.claude/agents"),
        )?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(root.join("home/.claude/agents"))?;
    fs::write(
        root.join("home/.claude/agents/user-helper.md"),
        "---\nname: user-helper\ndescription: User Claude helper\nmodel: inherit\n---\nUSER HELPER BODY\n",
    )?;
    let provider = ProviderFixture::start(vec![Script::Success]);
    let mut service = spawn(&root, provider.address);
    let address = await_ready(&root, &mut service);
    let inventory = json(address, "GET", "/api/agent", "");
    let home = root.join("home");
    let home_inventory = json(
        address,
        "GET",
        &format!(
            "/api/agent?location[directory]={}",
            percent_encode(home.to_str().ok_or("home path")?)
        ),
        "",
    );
    assert!(home_inventory["data"]
        .as_array()
        .ok_or("home roster")?
        .iter()
        .any(|agent| agent["name"] == "user-helper" && agent["source"] == "claude-user"));
    let rows = inventory["data"].as_array().ok_or("agent roster missing")?;
    let row = |name: &str| -> &serde_json::Value {
        rows.iter()
            .find(|agent| agent["name"] == name)
            .unwrap_or_else(|| panic!("{name} missing from the roster"))
    };
    let helper = row("claude-helper");
    assert_eq!(helper["source"], "claude-project");
    assert_eq!(
        helper["system"],
        "CLAUDE-HELPER: report findings as plain text, without edits.\n"
    );
    assert!(helper["description"]
        .as_str()
        .is_some_and(|text| text.contains("dropped unknown tools: Bash, Grep")));
    assert_eq!(row("user-helper")["source"], "claude-user");
    assert_eq!(row("worker")["source"], "builtin");
    assert_eq!(
        row("worker")["system"],
        "Complete the assigned work; preserve unrelated edits by other workers."
    );
    let session = create(address);
    assert_eq!(
        request(
            address,
            "POST",
            &format!("/api/session/{session}/agent"),
            r#"{"agent":"claude-helper"}"#
        )
        .0,
        204
    );
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"Report on the workspace"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies);
    let request = serde_json::from_str::<serde_json::Value>(&bodies[0])?;
    assert_eq!(
        request["model"], "gpt-live",
        "the sonnet alias inherits the session model, it does not route"
    );
    let system = request["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["role"] == "system")
        .cloned()
        .collect::<Vec<_>>();
    assert!(serde_json::to_string(&system)?
        .contains("CLAUDE-HELPER: report findings as plain text, without edits."));
    assert!(
        !request["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|tool| tool["name"] == "exec_command"),
        "the read-only ceiling must exclude the shell"
    );
    drop(bodies);
    service.stop_after_witness(&root);
    Ok(())
}

#[test]
fn forks_of_primary_agents_keep_their_mode_and_enforce_the_agent_step_cap(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let profile = root("primary-agent-forks");
    let _profile_cleanup = terminal_coordination::RootCleanup(profile.clone());
    prepare(&profile, 120_000);
    let path = profile.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    config["agents"] = serde_json::json!({"build":{"steps":1},"plan":{"steps":1}});
    fs::write(path, serde_json::to_vec(&config)?)?;
    let partial = format!(
        "data: {}\n\ndata: {}\n\ndata: {}\n\n",
        serde_json::json!({"type":"response.output_text.delta","delta":"bounded fork answer"}),
        serde_json::json!({"type":"response.output_text.done"}),
        serde_json::json!({"type":"response.incomplete","response":{"usage":{"input_tokens":31,"output_tokens":7,"total_tokens":38},"incomplete_details":{"reason":"max_output_tokens"}}})
    );
    let provider = ProviderFixture::start(vec![
        Script::Raw(partial.clone()),
        Script::Raw(partial),
        Script::Success,
    ]);
    let mut service = spawn(&profile, provider.address);
    let address = await_ready(&profile, &mut service);
    for role in ["build", "plan"] {
        let parent = create(address);
        let selection = serde_json::json!({"agent":role}).to_string();
        assert_eq!(
            request(
                address,
                "POST",
                &format!("/api/session/{parent}/agent"),
                &selection
            )
            .0,
            204
        );
        let fork = json(
            address,
            "POST",
            &format!("/api/session/{parent}/fork"),
            "{}",
        );
        let session = fork["data"]["id"].as_str().ok_or("fork absent")?;
        assert_eq!(
            request(
                address,
                "POST",
                &format!("/api/session/{session}/agent"),
                &selection
            )
            .0,
            204
        );
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"text":"Continue the fork"}"#,
        );
        permission_residuals::wait_session_idle(address, session);
        let db = rusqlite::Connection::open_with_flags(
            profile.join("database.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let failure: String = db.query_row("SELECT json_extract(data, '$.error.type') FROM event WHERE aggregate_id=?1 AND type='session.execution.failed.1' ORDER BY seq DESC LIMIT 1",[session],|row|row.get(0))?;
        assert_eq!(failure, "StepBudgetExhausted");
    }
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    service.stop_after_witness(&profile);
    Ok(())
}

#[test]
fn plan_cannot_move_its_write_root_through_a_directory_link(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    for target in [".", "src"] {
        let profile = root("plan-link-service");
        let _profile_cleanup = terminal_coordination::RootCleanup(profile.clone());
        prepare(&profile, 120_000);
        let workspace = root("plan-link-workspace");
        let _workspace_cleanup = terminal_coordination::RootCleanup(workspace.clone());
        process_fixture::cleanup(&workspace);
        fs::create_dir_all(workspace.join("src"))?;
        let link = workspace.join("plans");
        #[cfg(unix)]
        std::os::unix::fs::symlink(workspace.join(target), &link)?;
        #[cfg(windows)]
        {
            let status =
                std::process::Command::new(std::env::var_os("ComSpec").ok_or("cmd unavailable")?)
                    .args(["/D", "/C", "mklink", "/J"])
                    .arg(&link)
                    .arg(workspace.join(target))
                    .status()?;
            assert!(status.success(), "fixture junction was not created");
        }
        let provider = ProviderFixture::start(vec![
            Script::ToolInput {
                call: "call_plan_link",
                tool: "write",
                arguments: serde_json::json!({"path":Path::new(target).join("escaped.md"),"content":"must not escape"}),
            },
            Script::Success,
        ]);
        let mut service = spawn(&profile, provider.address);
        let address = await_ready(&profile, &mut service);
        let created = json(
            address,
            "POST",
            "/api/session",
            &serde_json::json!({"location":{"directory":workspace}}).to_string(),
        );
        let session = created["data"]["id"].as_str().ok_or("session absent")?;
        assert_eq!(
            request(
                address,
                "POST",
                &format!("/api/session/{session}/agent"),
                r#"{"agent":"plan"}"#
            )
            .0,
            204
        );
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"text":"Write a plan"}"#,
        );
        permission_residuals::wait_session_idle(address, session);
        assert!(!workspace.join(target).join("escaped.md").try_exists()?);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        service.stop_after_witness(&profile);
        #[cfg(unix)]
        fs::remove_file(link)?;
        #[cfg(windows)]
        fs::remove_dir(link)?;
        process_fixture::cleanup(&workspace);
    }
    Ok(())
}
