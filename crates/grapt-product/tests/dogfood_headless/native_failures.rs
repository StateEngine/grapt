//! Census regressions reach both the next model call and public transcript.
use super::*;

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn native_failure_causes_reach_model_and_transcript_without_stderr_credentials(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("native-failure-causes");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 30_000);
    unattended(&root);
    fs::write(root.join("binary.rs"), b"binary\0data")?;
    let command = if cfg!(windows) {
        "echo NATIVE_STDERR 1>&2 & echo api_key=PRIVATE_STDERR_KEY 1>&2 & ping -n 6 127.0.0.1 >nul"
    } else {
        "printf 'NATIVE_STDERR\\napi_key=PRIVATE_STDERR_KEY\\n' >&2; sleep 5"
    };
    let provider = ProviderFixture::start(vec![
        Script::Raw(skill_activation::tool_call(
            "call_native_timeout",
            "shell",
            serde_json::json!({"command":command,"timeout_ms":1000}),
        )),
        Script::Raw(skill_activation::tool_call(
            "call_native_exec",
            "exec_command",
            serde_json::json!({"cmd":"echo witness","shell":"relative/sh"}),
        )),
        Script::Raw(skill_activation::tool_call(
            "call_native_read",
            "read",
            serde_json::json!({"path":"binary.rs","offset":1,"limit":60}),
        )),
        Script::Success,
    ]);
    let mut service = spawn(&root, provider.address);
    let address = await_ready(&root, &mut service);
    let session = create(address);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_native_causes","text":"inspect tool failures","delivery":"queue","resume":true}"#,
    );
    for call in ["call_native_timeout", "call_native_read"] {
        compaction_continuity::approve(address, &session, call);
    }
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies).clone();
    assert_eq!(bodies.len(), 4);
    let initial: serde_json::Value = serde_json::from_str(&bodies[0])?;
    let description = initial["tools"]
        .as_array()
        .ok_or("provider tools missing")?
        .iter()
        .find(|tool| tool["name"] == "exec_command")
        .and_then(|tool| tool["description"].as_str())
        .ok_or("exec shell guidance missing")?;
    assert!(description.contains("Available shells: "), "{description}");
    assert!(description.len() <= 512, "{description}");
    for (index, call, tool, cause) in [
        (1, "call_native_timeout", "shell", "timed out after 1000 ms"),
        (
            2,
            "call_native_exec",
            "exec_command",
            "use a supported shell name or an absolute path",
        ),
        (3, "call_native_read", "read", "marks it binary"),
    ] {
        let body: serde_json::Value = serde_json::from_str(&bodies[index])?;
        let output = body["input"]
            .as_array()
            .ok_or("model input")?
            .iter()
            .find(|item| item["type"] == "function_call_output" && item["call_id"] == call)
            .and_then(|item| item["output"].as_str())
            .ok_or("model diagnostic missing")?;
        assert!(
            output.contains(&format!("{tool} failed")) && output.contains(cause),
            "{output}"
        );
        assert!(!output.contains("PRIVATE_STDERR_KEY"));
        if tool == "exec_command" {
            assert!(output.contains("relative/sh"), "{output}");
            assert!(output.contains("available shells: "), "{output}");
        }
        let history = json(
            address,
            "GET",
            &format!("/api/session/{session}/message?order=asc"),
            "",
        );
        let state = history["data"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|message| message["content"].as_array())
            .flatten()
            .find(|part| part["type"] == "tool" && part["id"] == call)
            .map(|part| &part["state"])
            .ok_or("transcript tool missing")?;
        assert_eq!(state["status"], "error");
        let detail = state["error"]["message"]
            .as_str()
            .ok_or("transcript diagnostic missing")?;
        assert!(detail.contains(cause), "{detail}");
        if tool == "exec_command" {
            assert!(detail.contains("relative/sh"), "{detail}");
            assert!(detail.contains("available shells: "), "{detail}");
        }
        assert!(!detail.contains("PRIVATE_STDERR_KEY"));
        assert!(detail.len() <= 2048);
        if tool == "shell" {
            assert!(detail.contains("NATIVE_STDERR"));
        }
    }
    service.stop_after_witness(&root);
    Ok(())
}
