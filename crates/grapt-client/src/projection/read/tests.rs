use super::*;

fn tool(name: &str, output: String) -> DurableTool {
    DurableTool {
        duration_ms: None,
        id: crate::wire::tool_call_id("call_000000000001")
            .unwrap_or_else(|error| panic!("read witness id: {error}")),
        name: name.into(),
        state: DurableToolState::Completed {
            input: r#"{"path":"src/é.rs","start_line":25,"line_count":100}"#.into(),
            output,
        },
        progress: Vec::new(),
    }
}

fn page() -> String {
    serde_json::json!({
        "path":"src/é.rs", "sha256":"MODEL_PATCH_HASH", "totalLines":50,
        "nextLine":41, "complete":false,
        "lines":(25..=40).map(|line| serde_json::json!({
            "line":line,"byteStart":line*2,"byteEnd":line*2+2,"text":format!("é source {line}")
        })).collect::<Vec<_>>()
    })
    .to_string()
}

#[test]
fn read_json_is_numbered_only_in_bounded_details_and_raw_output_is_unchanged() {
    let raw = page();
    let tool = tool("read_lines", raw.clone());
    let (text, start) = present(&tool).unwrap_or_else(|| panic!("read presentation missing"));
    assert_eq!(&text[..start], "Read src/é.rs · lines 25–40 (16 lines)");
    assert!(text.contains("25: é source 25") && text.contains("36: é source 36"));
    assert!(!text.contains("37: é source 37"));
    assert!(
        text.contains("… 4 more lines omitted") && text.contains("Continue reading at line 41")
    );
    assert!(!text.contains("byteStart") && !text.contains("MODEL_PATCH_HASH"));
    assert_eq!(
        tool.state,
        DurableToolState::Completed {
            input: r#"{"path":"src/é.rs","start_line":25,"line_count":100}"#.into(),
            output: raw
        }
    );
}

#[test]
fn empty_plain_and_malformed_reads_are_distinct() {
    let empty = tool(
        "read_lines",
        serde_json::json!({"lines":[],"totalLines":0,"nextLine":null,"complete":true}).to_string(),
    );
    assert_eq!(
        present(&empty).map(|(text, _)| text),
        Some("Read src/é.rs · 0 lines".into())
    );
    let plain = tool(
        "read",
        "25: é source\n26: more\n[sha256 for patch: SECRET_MACHINE_METADATA]\n".into(),
    );
    let (text, _) = present(&plain).unwrap_or_else(|| panic!("numbered read missing"));
    assert!(text.contains("lines 25–26 (2 lines)") && text.contains("25: é source"));
    assert!(!text.contains("SECRET_MACHINE_METADATA"));
    let continued = tool(
        "read",
        "25: source\n[Output truncated. Continue reading with offset: 26]\n".into(),
    );
    assert!(
        present(&continued).is_some_and(|(text, _)| text.contains("Continue reading at line 26"))
    );
    let invalid_continuation = tool(
        "read",
        "25: source\n[Output truncated. Continue reading with offset: 0]\n".into(),
    );
    assert!(present(&invalid_continuation)
        .is_some_and(|(text, _)| text.contains("output format unavailable")));
    let malformed = tool("read_lines", "{\"lines\":[".into());
    let (text, start) = present(&malformed).unwrap_or_else(|| panic!("raw fallback missing"));
    assert_eq!(&text[..start], "Read src/é.rs · output format unavailable");
    assert!(text.contains("Raw output preview:") && text.contains("{\"lines\":["));
    assert!(!text.contains("remaining output omitted"));
    let long = tool(
        "read_lines",
        (1..=13).map(|line| format!("raw {line}\n")).collect(),
    );
    let (text, _) = present(&long).unwrap_or_else(|| panic!("bounded raw fallback missing"));
    assert!(text.contains("raw 12") && !text.contains("raw 13"));
    assert!(text.contains("remaining output omitted"));
    let invalid = tool("read_lines", serde_json::json!({"lines":[{"line":0,"text":"not a line"}],"totalLines":1,"nextLine":null,"complete":true}).to_string());
    assert!(present(&invalid).is_some_and(|(text, _)| text.contains("output format unavailable")));
}

#[test]
fn read_relist_retains_one_bounded_slice_and_details_never_fabricate_completion() {
    let session = crate::wire::session_id("ses_000000000001")
        .unwrap_or_else(|error| panic!("read witness session: {error}"));
    let bounds = crate::bounds::Bounds {
        max_slice_bytes: 64,
        ..crate::bounds::Bounds::PRODUCTION
    };
    let mut view = crate::projection::SessionView::new(&session);
    let mut read = tool("read_lines", page());
    view.push_tool(&read, bounds);
    view.push_tool(&read, bounds);
    assert_eq!(view.slices.len(), 1);
    let slice = &view.slices[0];
    assert!(slice.text.len() <= bounds.max_slice_bytes && slice.truncated_bytes > 0);
    assert!(slice
        .read_detail_start
        .is_some_and(|start| slice.text.is_char_boundary(start)));
    read.state = DurableToolState::Running {
        input: r#"{"path":"src/é.rs"}"#.into(),
    };
    view.push_tool(&read, bounds);
    assert!(
        view.slices[0].closed,
        "stale running re-list resurrected a completed read"
    );
    let mut running = crate::projection::SessionView::new(&session);
    running.push_tool(&read, bounds);
    assert!(!running.slices[0].closed && running.slices[0].read_detail_start.is_none());
}

#[test]
fn decoded_paths_and_source_cannot_paint_terminal_controls_or_extra_summary_rows() {
    let mut read = tool("read_lines", serde_json::json!({
        "lines":[{"line":1,"text":"a\tb\u{1b}[31m"}],"totalLines":1,"nextLine":null,"complete":true
    }).to_string());
    if let DurableToolState::Completed { input, .. } = &mut read.state {
        *input = serde_json::json!({"path":"src/é\n.rs\u{1b}"}).to_string();
    }
    let (shown, start) = present(&read).unwrap_or_else(|| panic!("safe read preview missing"));
    assert!(!shown.contains('\u{1b}') && !shown[..start].contains('\n'));
    assert!(shown.contains("src/é\\n.rs") && shown.contains("1: a\\tb"));
}
