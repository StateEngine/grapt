//! An unsuccessful command is useful evidence for the next model request.
use super::*;

#[test]
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
fn model_receives_diff_and_failing_test_output_after_nonzero_shell_exits(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("shell-exit-model-output");
    prepare(&root, 120_000);
    unattended(&root);
    fs::write(root.join("before.txt"), "before\n")?;
    fs::write(root.join("after.txt"), "after\n")?;
    fs::write(
        root.join("failed_test.py"),
        "import unittest\nclass RedTest(unittest.TestCase):\n    def test_red(self):\n        self.fail('SHELL_TEST_FAILURE_WITNESS')\n",
    )?;
    let provider = ProviderFixture::start(vec![
        Script::ToolShell {
            call: "call_shell_zero",
            command: "printf hello",
        },
        Script::ToolShell {
            call: "call_shell_diff",
            command: "git diff --no-index -- before.txt after.txt",
        },
        Script::ToolShell {
            call: "call_shell_red_test",
            command: "/usr/bin/python3 -m unittest failed_test.py",
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
        r#"{"id":"msg_shell_exit_output","text":"inspect the diff and failing test","delivery":"queue","resume":true}"#,
    );
    for call in ["call_shell_zero", "call_shell_diff", "call_shell_red_test"] {
        compaction_continuity::approve(address, &session, call);
    }
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies).clone();
    assert_eq!(bodies.len(), 4, "shell result must continue to the model");
    let requests = bodies
        .iter()
        .map(|body| serde_json::from_str::<serde_json::Value>(body))
        .collect::<Result<Vec<_>, _>>()?;
    // Optional evidence capture holds actual request bodies, never headers.
    // Capture before assertions so the same witness preserves a mutant's loss.
    if let Some(path) = std::env::var_os("GRAPT_SHELL_EXIT_CAPTURE") {
        let capture = serde_json::to_vec_pretty(&requests)?;
        assert!(capture.len() <= 6 * 1024 * 1024);
        fs::write(path, capture)?;
    }
    let result = |request: &serde_json::Value, call: &str| -> Result<String, String> {
        request["input"]
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item["type"] == "function_call_output" && item["call_id"] == call)
            })
            .and_then(|item| item["output"].as_str())
            .map(|raw| {
                serde_json::from_str::<serde_json::Value>(raw)
                    .ok()
                    .and_then(|receipt| {
                        receipt
                            .get("output")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| raw.to_owned())
            })
            .ok_or_else(|| format!("model request omitted result for {call}: {request}"))
    };
    // Frozen42e4888cc0 tool-shell witness: plain hello plus a separate
    // status part. Our provider format carries one scalar, so join parts as
    // paragraphs in the oracle order rather than fusing a header into stdout.
    let notice = "[shell: this host has no cgroup containment; processes that leave the command's process group (setsid, daemons) may outlive it]";
    let ordinary = "hello\n\nCommand exited with code 0.";
    let fallback = format!("hello\n\n{notice}\n\nCommand exited with code 0.");
    let zero = result(&requests[1], "call_shell_zero")?;
    match std::env::var("GRAPT_SHELL_EXPECT_CONTAINMENT").as_deref() {
        Ok("contained") => assert_eq!(zero, ordinary),
        Ok("fallback") => assert_eq!(zero, fallback),
        Err(std::env::VarError::NotPresent) => assert!(
            zero == ordinary || zero == fallback,
            "unexpected model parts: {zero}"
        ),
        other => panic!("invalid containment witness mode: {other:?}"),
    }
    let diff = result(&requests[2], "call_shell_diff")?;
    assert!(
        diff.ends_with("\n\nCommand exited with code 1.")
            && diff.contains("@@")
            && diff.contains("-before")
            && diff.contains("+after"),
        "SHELL_MODEL_DIFF_OUTPUT_LOST: {diff}"
    );
    let test = result(&requests[3], "call_shell_red_test")?;
    assert!(
        test.ends_with("\n\nCommand exited with code 1.")
            && test.contains("SHELL_TEST_FAILURE_WITNESS")
            && test.contains("FAILED (failures=1)"),
        "SHELL_MODEL_TEST_OUTPUT_LOST: {test}"
    );
    let messages = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc"),
        "",
    );
    let tools = messages["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter(|part| part["type"] == "tool")
        .collect::<Vec<_>>();
    assert_eq!(tools.len(), 3);
    for (tool, code) in tools.into_iter().zip([0, 1, 1]) {
        assert_eq!(tool["state"]["status"], "completed");
        assert!(tool["state"]["content"]
            .to_string()
            .contains(&format!("Command exited with code {code}.")));
    }
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}
