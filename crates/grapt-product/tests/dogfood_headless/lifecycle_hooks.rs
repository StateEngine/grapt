//! Public HTTP input, real provider tool calls and actual configured argv hooks.
use super::*;
use serde_json::{json as value, Value};

pub(super) fn python() -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new(if cfg!(windows) { "python" } else { "python3" })
        .args(["-I", "-c", "import sys; print(sys.executable)"])
        .output()?;
    if !output.status.success() {
        return Err("Python hook fixture interpreter unavailable".into());
    }
    let executable = String::from_utf8(output.stdout)?.trim().to_owned();
    if !Path::new(&executable).is_absolute() {
        return Err("Python fixture is not absolute".into());
    }
    Ok(executable)
}

fn configure(root: &Path, mode: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let script = root.join("hook.py");
    fs::write(
        &script,
        r#"import json, os, sys, time
v = json.load(sys.stdin)
assert set(v) <= {'hook_event_name', 'session_id', 'cwd', 'tool_name', 'tool_use_id'}
assert not any(n in os.environ for n in ['SC_HOOK_API_KEY', 'SC_HOOK_AUTH'])
assert not any(n.upper().startswith(('STATEENGINE_', 'SECODE_STATEENGINE_')) or n.upper() == 'SECODE_SESSION_ID' for n in os.environ)
with open(sys.argv[1], 'a', encoding='utf-8') as f:
    f.write(json.dumps(v) + '\n')
event = v['hook_event_name']
mode = sys.argv[2]
if event == 'PreToolUse':
    if mode == 'exit-deny':
        print('policy exit refusal', file=sys.stderr)
        sys.exit(2)
    if mode == 'json-deny':
        print(json.dumps({'hookSpecificOutput': {'permissionDecision': 'deny', 'permissionDecisionReason': 'policy JSON refusal'}}))
        sys.exit(0)
    if mode == 'timeout':
        time.sleep(10)
    if mode == 'crash':
        sys.exit(7)
if event == 'PostToolUse' and mode == 'post-crash':
    sys.exit(7)
print(json.dumps({'hookSpecificOutput': {'permissionDecision': 'allow', 'additionalContext': event + ' annotation'}}))
"#,
    )?;
    let log = root.join("hook-events.jsonl");
    let profile = root.join("grapt.jsonc");
    let mut config: Value = serde_json::from_slice(&fs::read(&profile)?)?;
    config["hook_commands"] = value!({"audit": {
        "command": [python()?, "-I", script, log, mode],
        "timeout_ms": if mode == "timeout" { 1500 } else { 10_000 }
    }});
    config["hooks"] = value!({
        "SessionStart":["audit"], "UserPromptSubmit":["audit"], "PreToolUse":["audit"],
        "PostToolUse":["audit"], "Stop":["audit"], "Notification":["audit"]
    });
    config["provider"]["credential"] = "env:SC_HOOK_AUTH".into();
    // Service launch removes inherited identity; every hook journey also checks
    // filtering after configured overrides, including case variants on POSIX.
    config["shell_environment_policy"] = value!({"set": {
        "SECODE_SESSION_ID": "configured-session-sentinel",
        "secode_session_id": "configured-case-session-sentinel",
        "STATEENGINE_FUTURE_TEST": "configured-identity-sentinel"
    }});
    fs::write(profile, serde_json::to_vec(&config)?)?;
    Ok(log)
}

fn events(log: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    fs::read_to_string(log)?
        .lines()
        .map(|line| {
            let v: Value = serde_json::from_str(line)?;
            Ok(v["hook_event_name"]
                .as_str()
                .ok_or("event missing")?
                .to_owned())
        })
        .collect()
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn journey(mode: &str) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root(&format!("lifecycle-hooks-{mode}"));
    prepare(&root, 240_000);
    let log = configure(&root, mode)?;
    let workspace = super::root(&format!("lifecycle-hooks-{mode}-workspace"));
    process_fixture::cleanup(&workspace);
    fs::create_dir_all(&workspace)?;
    fs::write(workspace.join("source.txt"), "original")?;
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_hook_edit",
            tool: "edit",
            arguments: value!({
                "path":"source.txt", "old_text":"original", "new_text":"changed"
            }),
        },
        Script::Success,
    ]);
    let mut child = process_fixture::spawn_controlled_with(
        &root,
        provider.address,
        &[
            ("SC_HOOK_AUTH", "provider-secret"),
            ("SC_HOOK_API_KEY", "credential-sentinel"),
            ("SECODE_SESSION_ID", "inherited-session-sentinel"),
            ("STATEENGINE_FUTURE_TEST", "inherited-identity-sentinel"),
        ],
    );
    let address = await_ready(&root, &mut child);
    let session = json(
        address,
        "POST",
        "/api/session",
        &value!({"location":{"directory":workspace}}).to_string(),
    )["data"]["id"]
        .as_str()
        .ok_or("session missing")?
        .to_owned();
    let response = json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_hook_public_input","text":"private-prompt-sentinel","delivery":"queue","resume":true}"#,
    );
    assert!(
        response.to_string().contains("msg_hook_public_input"),
        "{response}"
    );
    permission_residuals::wait_session_idle(address, &session);
    let event_names = events(&log)?;
    let expected = if matches!(mode, "success" | "post-crash") {
        vec![
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "Stop",
            "Notification",
        ]
    } else {
        vec![
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "Stop",
            "Notification",
        ]
    };
    assert_eq!(event_names, expected, "{mode}");
    let log_text = fs::read_to_string(&log)?;
    for private in [
        "private-prompt-sentinel",
        "credential-sentinel",
        "identity-sentinel",
        "session-sentinel",
        "original",
        "changed",
    ] {
        assert!(!log_text.contains(private), "hook stdin leaked {private}");
    }
    let changed = matches!(mode, "success" | "post-crash");
    assert_eq!(
        fs::read_to_string(workspace.join("source.txt"))?,
        if changed { "changed" } else { "original" }
    );
    let bodies = locked(&provider.bodies).clone();
    assert_eq!(
        bodies.len(),
        2,
        "a tool refusal must reach the model, not abort the conversation"
    );
    let request: Value = serde_json::from_str(&bodies[1])?;
    let output = request["input"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|v| v["type"] == "function_call_output" && v["call_id"] == "call_hook_edit")
        .and_then(|v| v["output"].as_str())
        .ok_or("model tool result missing")?;
    let expected_detail = match mode {
        "exit-deny" => "policy exit refusal",
        "json-deny" => "policy JSON refusal",
        "timeout" => "timed out",
        "crash" => "exit code 7",
        "post-crash" => "effect already ran",
        _ => "PostToolUse annotation",
    };
    assert!(output.contains(expected_detail), "{mode}: {output}");
    if !changed {
        assert!(output.contains("no effect ran"), "{output}");
    }
    if mode == "success" {
        assert!(bodies[0].contains("SessionStart annotation"));
        assert!(bodies[0].contains("UserPromptSubmit annotation"));
    }
    let transcript = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    )
    .to_string();
    assert!(
        transcript.contains(expected_detail),
        "hook reason missing from transcript: {transcript}"
    );
    assert!(
        transcript.contains("lifecycleHook"),
        "hook outcomes not durable"
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&workspace);
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
fn lifecycle_six_events_at_provider_and_effect_boundaries() -> Result<(), Box<dyn std::error::Error>>
{
    journey("success")
}
#[test]
fn lifecycle_exit_two_refuses_effect_with_reason_to_model_and_transcript(
) -> Result<(), Box<dyn std::error::Error>> {
    journey("exit-deny")
}
#[test]
fn lifecycle_json_denial_refuses_effect_with_reason_to_model_and_transcript(
) -> Result<(), Box<dyn std::error::Error>> {
    journey("json-deny")
}
#[test]
fn lifecycle_pre_timeout_is_visible_and_runs_no_effect() -> Result<(), Box<dyn std::error::Error>> {
    journey("timeout")
}
#[test]
fn lifecycle_pre_crash_is_visible_and_runs_no_effect() -> Result<(), Box<dyn std::error::Error>> {
    journey("crash")
}
#[test]
fn lifecycle_post_crash_preserves_completed_effect_and_warns_against_retry(
) -> Result<(), Box<dyn std::error::Error>> {
    journey("post-crash")
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn v2_tool_call(mode: &str) -> Result<(String, String, bool), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root(&format!("hooks-v2-{mode}"));
    prepare(&root, 240_000);
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace)?;
    fs::write(workspace.join("source.txt"), "original")?;
    let script = root.join("v2.py");
    let log = root.join("v2-called");
    fs::write(
        &script,
        r#"import json, sys
v = json.load(sys.stdin)
if v['hook_event_name'] == 'WaitingForInput':
    if v['cwd'].endswith('notifier-workspace'):
        print('expected advisory notifier failure', file=sys.stderr)
        sys.exit(7)
    sys.exit(0)
with open(sys.argv[2], 'w', encoding='utf-8') as f:
    f.write(v['tool_name'])
assert v['tool_input']['path'] == 'source.txt'
mode = sys.argv[1]
if v['hook_event_name'] == 'PermissionRequest':
    if mode == 'request-timeout':
        import time
        time.sleep(10)
    decision = {'behavior': 'deny' if mode == 'request-deny' else 'allow', 'message': 'approval hook refused this edit'}
    if mode == 'request-invalid':
        decision['updatedPermissions'] = []
    print(json.dumps({'hookSpecificOutput': {'hookEventName': 'PermissionRequest', 'decision': decision}}))
    sys.exit(0)
assert v['hook_event_name'] == 'PreToolUse'
if mode in ('modify', 'invalid', 'replaced'):
    updated = dict(v['tool_input'])
    updated['new_text'] = 'formatted by hook' if mode in ('modify', 'replaced') else 7
    result = {'permissionDecision': 'modify', 'updatedInput': updated}
else:
    decision = 'allow' if mode in ('allow', 'ceiling') else mode
    result = {'permissionDecision': decision, 'permissionDecisionReason': 'hook policy refused this edit'}
print(json.dumps({'hookSpecificOutput': dict(hookEventName='PreToolUse', **result)}))
"#,
    )?;
    let python = python()?;
    let invoke = |mode: &str| {
        format!(
            "{}'{}' -I '{}' '{}' '{}'",
            if cfg!(windows) { "& " } else { "" },
            python,
            script.display(),
            mode,
            log.display()
        )
    };
    let mut hooks = Vec::new();
    if mode == "replaced" {
        hooks.push(value!({"type":"command", "command":invoke("allow")}));
    }
    hooks.push(
        value!({"type":"command", "command":invoke(if mode == "project" {"allow"} else if mode == "attended-ask" {"ask"} else {mode})}),
    );
    let profile = root.join("grapt.jsonc");
    let mut config: Value = serde_json::from_slice(&fs::read(&profile)?)?;
    config["hooks"] = value!({"PreToolUse":[{
        "matcher":if mode == "unmatched" {"Bash"} else {"Edit|Write"},
        "hooks":hooks
    }]});
    if mode.starts_with("request-") {
        config["hooks"] = value!({"PermissionRequest":[{
            "matcher":"Edit", "hooks":[{"type":"command", "command":invoke(mode), "timeout":1}]
        }]});
        config["permission"] = value!({"edit":if mode == "request-ceiling" {"deny"} else {"ask"}});
        if mode == "request-pre-ask" {
            config["hooks"]["PreToolUse"] = value!([{
                "matcher":"Edit", "hooks":[{"type":"command", "command":invoke("ask")}]
            }]);
        }
        if mode == "request-project" {
            config["hooks"] = value!({});
            config["hook_commands"] = value!({"approval": {
                "command":[python.clone(), "-I".to_owned(), script.display().to_string(), mode.to_owned(), log.display().to_string()]
            }});
            let directory = workspace.join(".grapt");
            fs::create_dir(&directory)?;
            fs::write(
                directory.join("grapt.jsonc"),
                serde_json::to_vec(&value!({
                    "hooks":{"PermissionRequest":[{"matcher":"Edit", "hooks":["approval"]}]}
                }))?,
            )?;
        }
    }
    if mode == "attended-ask" {
        config["hooks"]["WaitingForInput"] = value!([{
            "matcher":"NeverATool", "hooks":[{"type":"command", "command":invoke("notifier")}]
        }]);
    }
    if matches!(mode, "allow" | "ceiling" | "replaced") {
        config["permission"] = value!({"edit":if mode == "ceiling" {"deny"} else {"ask"}});
    }
    if mode == "project" {
        config["hook_commands"] = value!({"modifier": {
            "command":[python, "-I", script, "modify", log], "timeout_ms":10000
        }});
        let directory = workspace.join(".grapt");
        fs::create_dir(&directory)?;
        fs::write(
            directory.join("grapt.jsonc"),
            serde_json::to_vec(&value!({
                "hooks":{"PreToolUse":[{"matcher":"Edit", "hooks":["modifier"]}]}
            }))?,
        )?;
    }
    fs::write(profile, serde_json::to_vec(&config)?)?;
    let mut scripts = vec![
        Script::ToolInput {
            call: "call_hooks_v2",
            tool: "edit",
            arguments: value!({
                "path":"source.txt", "old_text":"original", "new_text":"changed"
            }),
        },
        Script::Success,
    ];
    if matches!(mode, "allow" | "request-allow") {
        scripts.extend([
            Script::ToolInput {
                call: "call_hooks_after_restart",
                tool: "edit",
                arguments: value!({
                    "path":"source.txt", "old_text":"changed", "new_text":"after restart"
                }),
            },
            Script::Success,
        ]);
    }
    let provider = ProviderFixture::start(scripts);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = json(
        address,
        "POST",
        "/api/session",
        &value!({"location":{"directory":workspace}}).to_string(),
    )["data"]["id"]
        .as_str()
        .ok_or("missing Session")?
        .to_owned();
    let _approver = (mode == "attended-ask").then(|| approval::Attachment::open(address, &session));
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"edit the file"}"#,
    );
    if mode == "attended-ask" {
        let row = permission_boundaries::pending(address, &session, "call_hooks_v2");
        let other_workspace = root.join("notifier-workspace");
        fs::create_dir(&other_workspace)?;
        let other = json(
            address,
            "POST",
            "/api/session",
            &value!({"location":{"directory":other_workspace}}).to_string(),
        )["data"]["id"]
            .as_str()
            .ok_or("notifier Session missing")?
            .to_owned();
        json(
            address,
            "POST",
            &format!("/api/session/{other}/form"),
            r#"{"title":"Name","fields":[{"key":"name","type":"string","required":true}]}"#,
        );
        let other_history =
            json(address, "GET", &format!("/api/session/{other}/message"), "").to_string();
        assert!(
            other_history.contains("hook failed with exit code 7"),
            "{other_history}"
        );
        let id = row["id"].as_str().ok_or("pending permission ID missing")?;
        let (status, _) = request(
            address,
            "POST",
            &format!("/api/session/{session}/permission/{id}/reply"),
            r#"{"reply":"once"}"#,
        );
        assert_eq!(status, 204);
    }
    permission_residuals::wait_session_idle(address, &session);
    let output = permission_boundaries::output(&provider, "call_hooks_v2");
    let text = fs::read_to_string(workspace.join("source.txt"))?;
    let called = log.is_file();
    let transcript = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    )
    .to_string();
    if called {
        assert!(
            transcript.contains("lifecycleHook"),
            "hook result was not retained"
        );
    }
    child.stop_after_witness(&root);
    drop(child);
    if matches!(mode, "allow" | "request-allow") {
        config["hooks"] = value!({});
        fs::write(root.join("grapt.jsonc"), serde_json::to_vec(&config)?)?;
        let mut restarted = spawn(&root, provider.address);
        let address = await_ready(&root, &mut restarted);
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"text":"continue after restart"}"#,
        );
        permission_residuals::wait_session_idle(address, &session);
        let after = permission_boundaries::output(&provider, "call_hooks_after_restart");
        assert!(
            after.contains("approval unavailable"),
            "hook allow survived removal/restart: {after}"
        );
        assert_eq!(
            fs::read_to_string(workspace.join("source.txt"))?,
            "changed",
            "old allow resurrected after restart"
        );
        restarted.stop_after_witness(&root);
        drop(restarted);
    }
    process_fixture::cleanup(&root);
    Ok((output, text, called))
}

#[test]
fn v2_allow_skips_asks_but_does_not_lift_denies_and_ask_forces_attendance(
) -> Result<(), Box<dyn std::error::Error>> {
    let (_, text, called) = v2_tool_call("allow")?;
    assert!(called);
    assert_eq!(text, "changed");
    for mode in ["ask", "deny", "ceiling", "replaced"] {
        let (output, text, called) = v2_tool_call(mode)?;
        assert!(called);
        assert_eq!(text, "original", "{mode}: {output}");
        assert!(
            output.contains(if matches!(mode, "ask" | "replaced") {
                "approval unavailable"
            } else if mode == "deny" {
                "hook policy refused"
            } else {
                "denied"
            }),
            "{output}"
        );
    }
    Ok(())
}

#[test]
fn v2_replacement_is_revalidated_and_nonmatching_tools_keep_the_original_input(
) -> Result<(), Box<dyn std::error::Error>> {
    let (_, text, called) = v2_tool_call("modify")?;
    assert!(called);
    assert_eq!(text, "formatted by hook");
    for mode in ["invalid", "unsupported"] {
        let (output, text, called) = v2_tool_call(mode)?;
        assert!(called);
        assert_eq!(text, "original");
        assert!(
            output.contains(if mode == "invalid" {
                "modified input rejected"
            } else {
                "unsupported decision"
            }),
            "{output}"
        );
    }
    let (_, text, called) = v2_tool_call("unmatched")?;
    assert!(!called);
    assert_eq!(text, "changed");
    Ok(())
}

#[test]
fn project_hook_cannot_replace_input_after_user_policy_checked_it(
) -> Result<(), Box<dyn std::error::Error>> {
    let (output, text, called) = v2_tool_call("project")?;
    assert!(called);
    assert_eq!(text, "original");
    assert!(
        output.contains("project hook cannot replace tool input"),
        "{output}"
    );
    Ok(())
}

pub(super) fn configure_events(
    root: &Path,
    names: &[&str],
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let script = root.join("event-hook.py");
    let log = root.join("advanced-events.jsonl");
    fs::write(
        &script,
        r#"import json, sys
v = json.load(sys.stdin)
with open(sys.argv[1], 'a', encoding='utf-8') as stream:
    stream.write(json.dumps(v) + '\n')
print(json.dumps({'hookSpecificOutput': {'hookEventName': v['hook_event_name']}}))
"#,
    )?;
    let command = format!(
        "{}'{}' -I '{}' '{}'",
        if cfg!(windows) { "& " } else { "" },
        python()?,
        script.display(),
        log.display()
    );
    let profile = root.join("grapt.jsonc");
    let mut config: Value = serde_json::from_slice(&fs::read(&profile)?)?;
    let mut hooks = serde_json::Map::new();
    for name in names {
        hooks.insert(
            (*name).into(),
            value!([{
                "matcher":"NeverATool", "hooks":[{"type":"command", "command":command}]
            }]),
        );
    }
    config["hooks"] = hooks.into();
    fs::write(profile, serde_json::to_vec(&config)?)?;
    Ok(log)
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn session_wait_and_subagent_events_use_their_real_lifetimes(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("advanced-hook-events");
    prepare(&root, 240_000);
    let log = configure_events(
        &root,
        &[
            "WaitingForInput",
            "SubagentStart",
            "SubagentStop",
            "SessionEnd",
        ],
    )?;
    let workspace = root.join("workspace");
    fs::create_dir(&workspace)?;
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_hook_child",
            tool: "subagent",
            arguments: value!({"task":"Say child completed"}),
        },
        Script::Success,
        Script::Success,
        Script::ToolInput {
            call: "call_hook_child_failure",
            tool: "subagent",
            arguments: value!({"task":"Run the failing child work"}),
        },
        Script::PartialThenMalformed,
        Script::Success,
    ]);
    let mut child = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = json(
        address,
        "POST",
        "/api/session",
        &value!({"location":{"directory":workspace}}).to_string(),
    )["data"]["id"]
        .as_str()
        .ok_or("Session missing")?
        .to_owned();
    let form = json(
        address,
        "POST",
        &format!("/api/session/{session}/form"),
        r#"{"title":"Name","fields":[{"key":"name","type":"string","required":true}]}"#,
    )["data"]["id"]
        .as_str()
        .ok_or("Form missing")?
        .to_owned();
    let transcript = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    )
    .to_string();
    assert!(
        transcript.contains("WaitingForInput") && transcript.contains("\"sessionOnly\":true"),
        "{transcript}"
    );
    let (status, _) = request(
        address,
        "POST",
        &format!("/api/session/{session}/form/{form}/reply"),
        r#"{"answer":{"name":"Ada"}}"#,
    );
    assert_eq!(status, 204);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"Delegate the child work"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"Delegate another child, which will fail"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    // Exercise normal shutdown; the generic process-loss fixture uses kill.
    drop(child.stdin.take());
    let status = PollDeadline::within(Duration::from_secs(15))
        .poll(Duration::from_millis(10), || {
            child
                .try_wait()
                .unwrap_or_else(|error| panic!("service wait failed: {error}"))
        })
        .ok_or("service did not finish its bounded shutdown")?;
    assert!(status.success(), "service shutdown failed: {status}");
    drop(child);
    let observed: Vec<Value> = fs::read_to_string(log)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    let started = observed
        .iter()
        .find(|row| row["hook_event_name"] == "SubagentStart")
        .ok_or("SubagentStart missing")?;
    let stopped = observed
        .iter()
        .find(|row| row["hook_event_name"] == "SubagentStop")
        .ok_or("SubagentStop missing")?;
    assert_ne!(started["session_id"], session);
    assert_eq!(started["session_id"], stopped["session_id"]);
    let starts: Vec<_> = observed
        .iter()
        .filter(|row| row["hook_event_name"] == "SubagentStart")
        .collect();
    let stops: Vec<_> = observed
        .iter()
        .filter(|row| row["hook_event_name"] == "SubagentStop")
        .collect();
    assert_eq!(
        starts.len(),
        2,
        "both successful and failed child must start"
    );
    assert_eq!(stops.len(), 2, "the failed child must also stop");
    assert_eq!(starts[1]["session_id"], stops[1]["session_id"]);
    assert_ne!(starts[0]["session_id"], starts[1]["session_id"]);
    let ends: Vec<_> = observed
        .iter()
        .filter(|row| row["hook_event_name"] == "SessionEnd")
        .collect();
    assert_eq!(
        ends.len(),
        1,
        "SessionEnd must be service-scoped, not emitted per Step or child"
    );
    assert_eq!(ends[0]["session_id"], session);
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
fn v2_ask_keeps_the_approver_across_receipts_and_unrelated_notifier_failure(
) -> Result<(), Box<dyn std::error::Error>> {
    let (_, text, called) = v2_tool_call("attended-ask")?;
    assert!(called);
    assert_eq!(text, "changed");
    Ok(())
}

#[path = "lifecycle_hooks/permission_request.rs"]
mod permission_request;
