//! A config-bound Session invokes built-ins through the ordinary permission path.
use super::*;

pub(super) fn bind_config(address: SocketAddr, root: &Path, session: &str) {
    stateengine_binding::bind(address, session, false);
    let path = root.join("private/session.json");
    fs::create_dir_all(path.parent().unwrap_or_else(|| panic!("config parent")))
        .unwrap_or_else(|error| panic!("config directory: {error}"));
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/stateengine-native/fixture_server.py");
    let config = serde_json::json!({"mcp":{"stateengine":{
        "type":"local", "command":["python3",script,"--mode","ready"],
        "environment":{"STATEENGINE_PROJECT_ID":"native-fixture",
        "STATEENGINE_AGENT_ID":"agt_native_fixture","SECODE_SESSION_ID":session}}}});
    fs::write(&path, config.to_string()).unwrap_or_else(|error| panic!("bound config: {error}"));
    grapt_login::profile_security::protect_directory(
        path.parent().unwrap_or_else(|| panic!("parent")),
    )
    .unwrap_or_else(|error| panic!("protect config directory: {error}"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .unwrap_or_else(|error| panic!("protect config: {error}"));
    }
    let opened = fs::File::open(&path).unwrap_or_else(|error| panic!("config open: {error}"));
    grapt_login::profile_security::protect_new_private_file(&path, &opened)
        .unwrap_or_else(|error| panic!("protect config file: {error}"));
    let (status, bytes) = request(
        address,
        "PUT",
        &format!("/api/session/{session}/stateengine/capability"),
        &serde_json::json!({"capability":"NATIVE_FIXTURE_CAPABILITY","configPath":path})
            .to_string(),
    );
    assert_eq!(
        status,
        204,
        "binding refused: {}",
        String::from_utf8_lossy(&bytes)
    );
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn bound_session_discovers_and_loads_an_installed_skill() {
    let _journey = process_journey_custody();
    for anthropic in [false, true] {
        let root = root(if anthropic {
            "bound-skill-anthropic"
        } else {
            "bound-skill-openai"
        });
        prepare(&root, 120_000);
        attended(&root);
        let skill = root.join(".agents/skills/stateengine/SKILL.md");
        fs::create_dir_all(skill.parent().unwrap_or_else(|| panic!("skill parent")))
            .unwrap_or_else(|error| panic!("skill directory: {error}"));
        fs::write(skill, format!("---\nname: stateengine\ndescription: StateEngine messages resume and inbox.\n---\n{}\nRead native mail through the governed tools.\n",skill_activation::BODY_MARKER))
            .unwrap_or_else(|error| panic!("installed skill: {error}"));
        if anthropic {
            context_quality::configure_anthropic(&root, 200_000);
        }
        let calls = [
            (
                "call_skill_dot",
                serde_json::json!({"id":".","file":".","query":"stateengine messages resume","cursor":0}),
            ),
            (
                "call_skill_x",
                serde_json::json!({"id":"x","file":"x","query":"stateengine","cursor":0}),
            ),
            (
                "call_bound_skill_discover",
                serde_json::json!({"id":" ","file":" ","query":"stateengine","cursor":0}),
            ),
            (
                "call_bound_skill_load",
                serde_json::json!({"id":"stateengine","file":null,"query":null,"cursor":null}),
            ),
        ];
        let mut scripts:Vec<_> = calls.into_iter().map(|(call,arguments)| {
            if anthropic {
                Script::Raw(context_quality::anthropic_stream(&[serde_json::json!({"type":"tool_use","id":call,"name":"skill","input":arguments})],"tool_use",serde_json::json!({"input_tokens":20})))
            } else { Script::ToolInput {call,tool:"skill",arguments} }
        }).collect();
        scripts.push(if anthropic {
            context_quality::anthropic_text("done", serde_json::json!({"input_tokens":20}))
        } else {
            Script::Success
        });
        let provider = ProviderFixture::start(scripts);
        let mut child = spawn(&root, provider.address);
        let address = await_ready(&root, &mut child);
        let _approver = approval::Attachment::open(address, "*");
        let session = stateengine_binding::create_native(address, &root);
        if anthropic {
            context_quality::select_anthropic(address, &session);
        }
        bind_config(address, &root, &session);
        json(address,"POST",&format!("/api/session/{session}/prompt"),
            &serde_json::json!({"id":"msg_bound_skill","text":"Use the installed skill.","delivery":"queue","resume":true}).to_string());
        compaction_continuity::approve_batch(
            address,
            &session,
            &["call_bound_skill_discover", "call_bound_skill_load"],
        );
        permission_residuals::wait_session_idle(address, &session);
        let bodies = locked(&provider.bodies);
        assert_eq!(bodies.len(), 5);
        for body in &bodies[1..3] {
            assert!(
                body.contains("skill discovery and loading are exclusive; omit id/file to search"),
                "{body}"
            );
        }
        assert!(
            bodies[3].contains("StateEngine messages resume and inbox."),
            "{}",
            bodies[3]
        );
        assert!(
            bodies[4].contains(skill_activation::BODY_MARKER),
            "{}",
            bodies[4]
        );
        let first: serde_json::Value =
            serde_json::from_str(&bodies[0]).unwrap_or_else(|e| panic!("body:{e}"));
        let tool = first["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|tool| tool["name"] == "skill")
            .unwrap_or_else(|| panic!("skill schema missing"));
        let schema = &tool[if anthropic {
            "input_schema"
        } else {
            "parameters"
        }];
        for field in ["id", "file", "query", "cursor"] {
            assert!(
                schema["properties"][field]["type"]
                    .as_array()
                    .is_some_and(|types| types.iter().any(|t| t == "null")),
                "{schema}"
            );
        }
        for keyword in ["oneOf", "anyOf", "allOf"] {
            assert!(schema.get(keyword).is_none(), "{schema}");
        }
        drop(bodies);
        child.stop_after_witness(&root);
        drop(child);
        process_fixture::cleanup(&root);
    }
}

#[test]
fn bound_session_shell_pipe_and_pty_stamp_only_their_own_identity() {
    let _journey = process_journey_custody();
    let root = root("bound-session-shell");
    prepare(&root, 120_000);
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(
        &fs::read(&path).unwrap_or_else(|error| panic!("config read: {error}")),
    )
    .unwrap_or_else(|error| panic!("config decode: {error}"));
    config["shell_environment_policy"] = serde_json::json!({"include_only":["*"], "set":{
        "SECODE_SESSION_ID":"launcher-session","STATEENGINE_SEAT_SURFACE":"codex",
        "STATEENGINE_AGENT_ID":"launcher-agent","SECODE_STATEENGINE_SEAT_LAUNCH_PIN":"launcher-pin",
        "CODEX_THREAD_ID":"launcher-codex","CLAUDE_CODE_SESSION_ID":"launcher-claude"}});
    fs::write(path, config.to_string()).unwrap_or_else(|error| panic!("config write: {error}"));
    let command = if cfg!(windows) {
        "if defined STATEENGINE_AGENT_ID exit /b 9\nif defined SECODE_STATEENGINE_SEAT_LAUNCH_PIN exit /b 9\nif defined CODEX_THREAD_ID exit /b 9\nif defined CLAUDE_CODE_SESSION_ID exit /b 9\necho SC_SESSION=%SECODE_SESSION_ID%\necho SC_SURFACE=%STATEENGINE_SEAT_SURFACE%"
    } else {
        "test -z \"${STATEENGINE_AGENT_ID+x}${SECODE_STATEENGINE_SEAT_LAUNCH_PIN+x}${CODEX_THREAD_ID+x}${CLAUDE_CODE_SESSION_ID+x}\" || exit 9; printf 'SC_SESSION=%s\\nSC_SURFACE=%s\\n' \"$SECODE_SESSION_ID\" \"$STATEENGINE_SEAT_SURFACE\""
    };
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_bound_shell",
            tool: "shell",
            arguments: serde_json::json!({"command":command}),
        },
        Script::ToolInput {
            call: "call_bound_pipe",
            tool: "exec_command",
            arguments: serde_json::json!({"cmd":command,"tty":false,"yield_time_ms":30000}),
        },
        Script::ToolInput {
            call: "call_bound_pty",
            tool: "exec_command",
            arguments: serde_json::json!({"cmd":command,"tty":true,"yield_time_ms":30000}),
        },
        Script::Success,
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = stateengine_binding::create_native(address, &root);
    bind_config(address, &root, &session);
    json(address,"POST",&format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":"msg_bound_shell","text":"Check child identity.","delivery":"queue","resume":true}).to_string());
    compaction_continuity::approve_batch(
        address,
        &session,
        &["call_bound_shell", "call_bound_pipe", "call_bound_pty"],
    );
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies);
    assert_eq!(bodies.len(), 4);
    for (index, call) in ["call_bound_shell", "call_bound_pipe", "call_bound_pty"]
        .iter()
        .enumerate()
    {
        let body: serde_json::Value = serde_json::from_str(&bodies[index + 1])
            .unwrap_or_else(|error| panic!("body: {error}"));
        let output = body["input"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|item| item["type"] == "function_call_output" && item["call_id"] == *call)
            .and_then(|item| item["output"].as_str())
            .unwrap_or_else(|| panic!("missing {call} output"));
        assert!(
            output.contains(&format!("SC_SESSION={session}")),
            "{call}: {output}"
        );
        assert!(output.contains("SC_SURFACE=secode"), "{call}: {output}"); // grapt-rename: keep
        assert!(!output.contains("launcher-"), "{call}: {output}");
    }
    drop(bodies);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

/// Requires the actual StateEngine checkout; never substitutes a mock authenticator.
#[test]
#[ignore = "set SECODE_STATEENGINE_SOURCE to a StateEngine checkout and run with --ignored"]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn native_bound_session_shell_authenticates_real_se_msg_list(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let source = PathBuf::from(
        std::env::var_os("SECODE_STATEENGINE_SOURCE")
            .ok_or("SECODE_STATEENGINE_SOURCE is required")?,
    );
    assert!(source.join("src/sender-authentication.mjs").is_file());
    let root = root("bound-session-real-se");
    prepare(&root, 120_000);
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/stateengine-native/shell_sender_fixture.mjs");
    let descriptor = root.join("sender-fixture.json");
    let command = format!(
        "node \"{}\" check \"{}\" \"{}\"",
        script.display(),
        source.display(),
        descriptor.display()
    );
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_real_se_shell",
            tool: "shell",
            arguments: serde_json::json!({"command":command}),
        },
        Script::ToolInput {
            call: "call_real_se_pipe",
            tool: "exec_command",
            arguments: serde_json::json!({"cmd":command,"tty":false,"yield_time_ms":30000}),
        },
        Script::ToolInput {
            call: "call_real_se_pty",
            tool: "exec_command",
            arguments: serde_json::json!({"cmd":command,"tty":true,"yield_time_ms":30000}),
        },
        Script::Success,
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = stateengine_binding::create_native(address, &root);
    bind_config(address, &root, &session);
    let mut setup = Command::new("node");
    setup
        .args([
            script.as_os_str(),
            "setup".as_ref(),
            source.as_os_str(),
            descriptor.as_os_str(),
            session.as_ref(),
        ])
        .stdin(Stdio::piped());
    grapt_service::launcher_environment::scrub(&mut setup);
    let mut fixture = process_fixture::spawn_captured(&root, &mut setup);
    let wait = PollDeadline::within(Duration::from_secs(30));
    assert!(
        wait.poll(Duration::from_millis(10), || {
            assert!(
                fixture.try_wait().is_ok_and(|exit| exit.is_none()),
                "real sender fixture exited"
            );
            descriptor.is_file().then_some(())
        })
        .is_some(),
        "real sender fixture was not ready"
    );
    json(address,"POST",&format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":"msg_real_se_shell","text":"List disposable fixture mail.","delivery":"queue","resume":true}).to_string());
    compaction_continuity::approve_batch(
        address,
        &session,
        &[
            "call_real_se_shell",
            "call_real_se_pipe",
            "call_real_se_pty",
        ],
    );
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies);
    assert_eq!(bodies.len(), 4);
    for (index, call) in [
        "call_real_se_shell",
        "call_real_se_pipe",
        "call_real_se_pty",
    ]
    .iter()
    .enumerate()
    {
        let body: serde_json::Value = serde_json::from_str(&bodies[index + 1])?;
        let output = body["input"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|item| item["type"] == "function_call_output" && item["call_id"] == *call)
            .and_then(|item| item["output"].as_str())
            .ok_or("real se output missing")?;
        assert!(
            output.contains("REAL_SE_MSG_LIST_AUTHENTICATED=restart-seat"),
            "{call}: {output}"
        );
    }
    drop(bodies);
    drop(fixture.stdin.take());
    let stopped = fixture.wait_with_output()?;
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}
