use super::*;
use crate::{wire, Bounds, SessionView};

fn report() -> Value {
    serde_json::json!({"status":"exited","exit_code":0,"done":true,
        "output":"first\nsecond\n", "has_more_output":false,"omitted_bytes":0})
}

#[test]
fn public_reports_keep_exit_omission_and_completion_distinct() {
    let mut value = report();
    value["exit_code"] = serde_json::json!(7);
    value["done"] = serde_json::json!(false);
    value["omitted_bytes"] = serde_json::json!(10);
    value["has_more_output"] = serde_json::json!(true);
    let shown = present("exec_command", &value.to_string()).unwrap_or_default();
    assert_eq!(shown, "\n  exit 7 · completion pending · 10 output bytes omitted · more output available\n  first\n  second");
    value["status"] = serde_json::json!("timed_out");
    let _ = value
        .as_object_mut()
        .map(|object| object.remove("exit_code"));
    value["cleanup_error"] = serde_json::json!("cleanup failed\nretry later");
    let shown = present("write_stdin", &value.to_string()).unwrap_or_default();
    assert!(shown.contains("timed out · completion pending"));
    assert!(shown.contains("cleanup_error: cleanup failed\\nretry later"));
    assert!(!shown.contains("exit 0"));
}

#[test]
fn unknown_or_malformed_reports_keep_the_raw_fallback() {
    for raw in ["plain shell output", "{bad json", r#"{"output":"text"}"#] {
        assert!(present("exec_command", raw).is_none());
    }
    assert!(present("another_tool", &report().to_string()).is_none());
    for (key, invalid) in [
        ("exit_code", Value::Null),
        ("done", Value::Null),
        ("has_more_output", serde_json::json!("false")),
        ("omitted_bytes", serde_json::json!(-1)),
        ("output", serde_json::json!(["text"])),
        ("status", serde_json::json!("unknown")),
    ] {
        let mut value = report();
        value[key] = invalid;
        assert!(
            present("exec_command", &value.to_string()).is_none(),
            "{key}"
        );
    }
}

#[test]
fn decoded_terminal_controls_stay_literal_and_crlf_stays_line_based() {
    let mut value = report();
    value["output"] = serde_json::json!("hello\r\n\u{1b}[2J\nprogress\rnext\tcolumn");
    let shown = present("exec_command", &value.to_string()).unwrap_or_default();
    assert_eq!(
        shown,
        "\n  exit 0\n  hello\n  \\u{1b}[2J\n  progress\\rnext\\tcolumn"
    );
    assert!(!shown.contains('\u{1b}'));
}

#[test]
fn audit_result_replays_as_literal_log_lines_with_approved_arguments(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/expanded-shell-audit.json"
    )))?;
    assert!(fixture["before_rows"]
        .as_array()
        .ok_or("frame rows missing")?
        .iter()
        .any(|row| row.as_str().is_some_and(|row| row.contains("\\ncheck_000"))));
    let captured = &fixture["tool"];
    let tool = wire::DurableTool {
        id: wire::tool_call_id(captured["id"].as_str().ok_or("id missing")?)?,
        name: captured["name"].as_str().ok_or("name missing")?.into(),
        duration_ms: None,
        progress: vec![],
        state: wire::DurableToolState::Completed {
            input: serde_json::to_string(&captured["state"]["input"])?,
            output: captured["state"]["content"][0]["text"]
                .as_str()
                .ok_or("output missing")?
                .into(),
        },
    };
    let mut view = SessionView::new(&wire::session_id("ses_000000000001")?);
    view.push_tool(&tool, Bounds::PRODUCTION);
    let slice = view
        .slices
        .iter()
        .find(|slice| slice.tool_id.is_some())
        .ok_or("tool missing")?;
    assert!(slice.text.contains("  cmd python3 -c"));
    assert!(slice.text.contains("  max_output_tokens 6000"));
    assert!(slice.text.contains(
        "\n  exit 0\n  Build validation log\n  check_000: passed — signed arithmetic validation\n"
    ));
    assert!(slice.text.contains(
        "\n  check_079: passed — signed arithmetic validation\n  All validation checks completed."
    ));
    assert!(!slice.text.contains("\\ncheck_000") && !slice.text.contains("\"cleanup\""));
    let summary = slice.tool_summary.as_ref().ok_or("summary missing")?;
    assert_eq!(summary.preview[0], "Build validation log");
    assert_eq!(summary.omitted_lines, 79);
    Ok(())
}

#[test]
fn decoded_output_still_uses_the_existing_utf8_slice_bound(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut value = report();
    value["output"] = serde_json::json!("中👩\n".repeat(100));
    let tool = wire::DurableTool {
        id: wire::tool_call_id("call_bounded_exec")?,
        name: "exec_command".into(),
        duration_ms: None,
        progress: vec![],
        state: wire::DurableToolState::Completed {
            input: r#"{"cmd":"printf"}"#.into(),
            output: value.to_string(),
        },
    };
    let mut view = SessionView::new(&wire::session_id("ses_000000000001")?);
    let bounds = Bounds {
        max_slice_bytes: 128,
        ..Bounds::PRODUCTION
    };
    view.push_tool(&tool, bounds);
    assert!(view.slices[0].text.len() <= 128);
    assert!(view.slices[0].truncated_bytes > 0);
    assert!(view.slices[0].text.contains("exit 0"));
    Ok(())
}
