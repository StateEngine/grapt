use super::*;
use crate::{Bounds, SessionView};
type Result = std::result::Result<(), Box<dyn std::error::Error>>;

fn tool(
    id: &str,
    name: &str,
    input: Value,
    output: Value,
) -> std::result::Result<DurableTool, Box<dyn std::error::Error>> {
    Ok(DurableTool {
        id: crate::wire::tool_call_id(id)?,
        name: name.into(),
        duration_ms: Some(1500),
        progress: vec![],
        state: DurableToolState::Completed {
            input: input.to_string(),
            output: output.to_string(),
        },
    })
}

#[test]
fn stdin_uses_the_matching_retained_command_title_and_keeps_raw_detail() -> Result {
    let mut view = SessionView::new(&crate::wire::session_id("ses_stdin_titles")?);
    for (id, operation, description) in [
        ("call_exec_a", "operation_a", "Check the build"),
        ("call_exec_b", "operation_b", "Watch tests"),
    ] {
        view.push_tool(
            &tool(
                id,
                "exec_command",
                serde_json::json!({"cmd":"cargo test", "description":description}),
                serde_json::json!({"session_id":operation,"output":"started"}),
            )?,
            Bounds::PRODUCTION,
        );
    }
    for (index, input, title, action) in [
        (
            0,
            serde_json::json!({"session_id":"operation_a"}),
            "Waited for Check the build output",
            StdinAction::Wait,
        ),
        (
            1,
            serde_json::json!({"session_id":"operation_b","chars":"yes\n"}),
            "Wrote to Watch tests",
            StdinAction::Write,
        ),
        (
            2,
            serde_json::json!({"session_id":"operation_a","chars":"", "description":"Sent input"}),
            "Waited for Check the build output",
            StdinAction::Wait,
        ),
        (
            3,
            serde_json::json!({"session_id":"unknown","chars":" "}),
            "Wrote to the running command",
            StdinAction::Write,
        ),
        (
            4,
            serde_json::json!({"chars":"\u{3}"}),
            "Wrote to the running command",
            StdinAction::Write,
        ),
    ] {
        let original = tool(
            &format!("call_stdin_{index}"),
            "write_stdin",
            input,
            serde_json::json!({"output":"retained output"}),
        )?;
        view.push_tool(&original, Bounds::PRODUCTION);
        let row = view.slices.last().ok_or("missing row")?;
        let summary = row.tool_summary.as_ref().ok_or("missing summary")?;
        assert_eq!(summary.title, title);
        assert_eq!(summary.stdin, Some(action));
        assert_eq!(summary.duration_ms, Some(1500));
        assert_eq!(summary.preview, ["retained output"]);
        assert!(row.text.contains("retained output"));
    }
    Ok(())
}

#[test]
fn unknown_or_malformed_input_cannot_claim_that_it_only_polled() -> Result {
    for input in [
        serde_json::json!({"chars":false}),
        serde_json::json!({"chars":null}),
        serde_json::json!([]),
    ] {
        let summary = summarize(&tool(
            "call_malformed_stdin",
            "write_stdin",
            input,
            serde_json::json!({}),
        )?);
        assert_eq!(summary.stdin, None);
        assert_eq!(summary.title, "Interacted with the running command");
    }
    let summary = summarize(&tool(
        "call_long_handle",
        "write_stdin",
        serde_json::json!({"session_id":"x".repeat(257),"chars":""}),
        serde_json::json!({}),
    )?);
    assert!(summary.terminal_session.is_none());
    assert_eq!(summary.title, "Waited for the running command output");
    Ok(())
}

#[test]
fn shell_rows_show_actual_enforcement_and_keep_off_unmarked() -> Result {
    for (receipt, label) in [
        (
            serde_json::json!({"kind":"platform_sandbox","backend":"mxc"}),
            "sandboxed",
        ),
        (
            serde_json::json!({"kind":"degraded","backend":"mxc","reason":"PSEC unavailable"}),
            "degraded: PSEC unavailable",
        ),
        (serde_json::json!({"kind":"harness_only"}), ""),
    ] {
        let row = tool(
            "call_enforcement",
            "shell",
            serde_json::json!({"cmd":"echo hello"}),
            serde_json::json!({"output":"hello\nworld","enforcement":receipt}),
        )?;
        let summary = summarize(&row);
        assert!(summary.result.ends_with(label));
        assert!(summary.result.starts_with("2 output lines"));
        if label.is_empty() {
            assert!(!summary.result.contains("sandbox") && !summary.result.contains("degraded"));
        }
    }
    let mut refused = tool(
        "call_unavailable",
        "exec_command",
        serde_json::json!({"cmd":"echo refused"}),
        serde_json::json!("unused"),
    )?;
    refused.state = DurableToolState::Error {
        input: "{}".into(), output: Some("startup diagnostics".into()),
        error: crate::wire::DurableError { kind: "failed".into(), message: serde_json::json!({"enforcement":{"kind":"degraded","backend":"mxc","reason":"PSEC unavailable"},"ran":false}).to_string() },
    };
    assert!(summarize(&refused)
        .result
        .contains("degraded: PSEC unavailable"));

    Ok(())
}
