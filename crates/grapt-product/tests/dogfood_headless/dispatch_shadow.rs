//! Real permission outcomes remain unchanged while normalized dispatches are observed.
use super::*;
const LOG: &str = "diagnostics/tool-dispatch-shadow.jsonl";
const PRIVATE: &str = "PRIVATE_SHADOW_FIXTURE_84029";

fn rows(root: &Path) -> Vec<serde_json::Value> {
    match fs::read_to_string(root.join(LOG)) {
        Ok(text) => text
            .rsplit_once('\n')
            .map_or("", |(complete, _)| complete)
            .lines()
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|error| panic!("malformed completed shadow record: {error}"))
            })
            .collect(), // Only the in-flight unterminated final line is retried.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("shadow observations unreadable: {error}"),
    }
}

fn wait_rows(
    root: &Path,
    expected: impl Fn(&[serde_json::Value]) -> bool,
) -> Vec<serde_json::Value> {
    let wait = PollDeadline::new();
    while wait.pending() {
        let rows = rows(root);
        if expected(&rows) {
            for row in &rows {
                assert!(!row.to_string().contains(PRIVATE));
                assert_eq!(row["routing"], "unmeasured");
            }
            return rows;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("required shadow observations did not arrive ({wait})");
}

#[test]
fn recording_and_an_unavailable_sink_preserve_allow_and_inspect_refusal() {
    let _journey = process_journey_custody();
    for (label, inspect, broken_sink) in [
        ("shadow-allowed", false, false),
        ("shadow-inspect", true, false),
        ("shadow-sink-unavailable", false, true),
    ] {
        let root = root(label);
        prepare(&root, 200_000);
        unattended(&root);
        if broken_sink {
            fs::create_dir_all(root.join(LOG)).unwrap_or_else(|error| panic!("{error}"));
        }
        let provider = ProviderFixture::start(vec![
            Script::ToolInput {
                call: "call_PRIVATE_SHADOW_FIXTURE_84029",
                tool: "shell",
                arguments: serde_json::json!({"command":format!("echo {PRIVATE}")}),
            },
            Script::SuccessText("finished"),
        ]);
        let mut child = spawn(&root, provider.address);
        let address = await_ready(&root, &mut child);
        let session = create(address);
        if inspect {
            let (status, _) = request(
                address,
                "POST",
                &format!("/api/session/{session}/preset"),
                r#"{"preset":"inspect"}"#,
            );
            assert_eq!(status, 204);
        }
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"text":"run the fixture command"}"#,
        );
        permission_residuals::wait_session_idle(address, &session);
        let bodies = locked(&provider.bodies);
        assert_eq!(
            bodies.len(),
            2,
            "the original tool outcome must reach the model"
        );
        let body: serde_json::Value = serde_json::from_str(&bodies[1])
            .unwrap_or_else(|error| panic!("provider body is not JSON: {error}"));
        let output = body["input"]
            .as_array()
            .and_then(|items| {
                items.iter().find(|item| {
                    item["type"] == "function_call_output"
                        && item["call_id"] == "call_PRIVATE_SHADOW_FIXTURE_84029"
                })
            })
            .and_then(|item| item["output"].as_str())
            .unwrap_or_else(|| panic!("the model did not receive the tool outcome"));
        if inspect {
            assert!(output.to_ascii_lowercase().contains("inspect"));
            assert!(!output.contains(PRIVATE));
        } else {
            assert!(
                output.contains(PRIVATE),
                "the diagnostic sink changed execution"
            );
        }
        drop(bodies);
        if broken_sink {
            assert!(root.join(LOG).is_dir());
        } else {
            let terminal = if inspect {
                "authority_unavailable"
            } else {
                "completed"
            };
            let rows = wait_rows(&root, |rows| {
                rows.iter().any(|row| row["phase"] == terminal)
            });
            assert_eq!(
                rows.iter().filter(|row| row["phase"] == "dispatch").count(),
                1
            );
            assert!(rows.iter().all(|row| row["session"] == session));
            assert_eq!(
                rows.iter().any(|row| row["phase"] == "effect_started"),
                !inspect
            );
            assert_eq!(
                rows.iter().any(|row| row["phase"] == "permission_granted"),
                !inspect
            );
        }
        child.stop_after_witness(&root);
        drop(child);
        process_fixture::cleanup(&root);
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn assert_nested_mail_dispatches(root: &Path) {
    let rows = wait_rows(root, |rows| {
        rows.iter()
            .filter(|row| row["origin"] == "nested" && row["phase"] == "completed")
            .count()
            == 2
    });
    assert_eq!(
        rows.iter()
            .filter(|row| row["origin"] == "nested" && row["phase"] == "dispatch")
            .count(),
        2
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row["origin"] == "direct"
                && row["tool"] == "codemode_envelope"
                && row["phase"] == "dispatch")
            .count(),
        2
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row["origin"] == "nested"
                && row["phase"] == "prepared"
                && row["shape"] == "mcp")
            .count(),
        2
    );
}

#[cfg(unix)]
#[test]
fn prepared_shape_observes_the_hook_replacement_without_retaining_either_payload() {
    let _journey = process_journey_custody();
    let root = root("shadow-after-hook");
    prepare(&root, 200_000);
    unattended(&root);
    let script = root.join("shadow-modifier.py");
    fs::write(&script, r#"import json,sys
event=json.load(sys.stdin)
updated=dict(event['tool_input'])
updated['command']='git status'
print(json.dumps({'hookSpecificOutput':{'hookEventName':'PreToolUse','permissionDecision':'modify','updatedInput':updated}}))
"#).unwrap_or_else(|error| panic!("hook fixture write: {error}"));
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(
        &fs::read(&path).unwrap_or_else(|error| panic!("hook profile read: {error}")),
    )
    .unwrap_or_else(|error| panic!("hook profile JSON: {error}"));
    let python = lifecycle_hooks::python()
        .unwrap_or_else(|error| panic!("hook fixture interpreter: {error}"));
    config["hook_commands"]["shadow-modifier"] =
        serde_json::json!({"command":[python,"-I",script]});
    config["hooks"]["PreToolUse"] =
        serde_json::json!([{"matcher":"Bash","hooks":["shadow-modifier"]}]);
    fs::write(&path, config.to_string())
        .unwrap_or_else(|error| panic!("hook profile write: {error}"));
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_PRIVATE_SHADOW_FIXTURE_84029",
            tool: "shell",
            arguments: serde_json::json!({"command":format!("echo {PRIVATE}")}),
        },
        Script::SuccessText("finished"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"run the fixture command"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    let rows = wait_rows(&root, |rows| {
        rows.iter().any(|row| row["phase"] == "effect_started")
    });
    let prepared: Vec<_> = rows
        .iter()
        .filter(|row| row["phase"] == "prepared")
        .collect();
    assert_eq!(prepared.len(), 1);
    assert_eq!(prepared[0]["shape"], "git_status");
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}
