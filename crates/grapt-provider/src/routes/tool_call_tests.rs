//! Responses function-call decoding: the durable call id travels, empty
//! arguments read as `{}`, a call is emitted exactly once, and a call's
//! complete arguments reported only as it finishes are not dropped.
use super::*;

#[test]
fn responses_decodes_a_tool_call_end_to_end() {
    // The gap the Codex adapter used to refuse outright. `item_id` is the
    // attempt-local accumulation key; `call_id` is what becomes durable, and
    // the assertion below is that the DURABLE id travels, not the item id.
    let harness = responses(&[
        r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"item_1","call_id":"call_abc","name":"read_file"}}"#,
        r#"{"type":"response.function_call_arguments.delta","item_id":"item_1","delta":"{\"path\":"}"#,
        r#"{"type":"response.function_call_arguments.delta","item_id":"item_1","delta":"\"a.txt\"}"}"#,
        r#"{"type":"response.function_call_arguments.done","item_id":"item_1"}"#,
        r#"{"type":"response.completed","response":{}}"#,
    ]);
    assert_eq!(
        harness.shape(),
        vec![
            "tool.open:call_abc:read_file",
            "tool.delta:call_abc:{\"path\":",
            "tool.delta:call_abc:\"a.txt\"}",
            "tool.close:call_abc",
            "tool.called:call_abc:read_file:{\"path\":\"a.txt\"}",
            "terminal:ToolCalls",
        ],
        "a completed Responses function call settled as Stop instead of requesting the bounded continuation Step"
    );
}

/// A call with empty arguments reached the tool and history as "", which is
/// not JSON. Empty arguments must read as `{}`.
#[test]
fn responses_reads_empty_tool_arguments_as_an_empty_object() {
    for arguments in [None, Some(""), Some("  \n")] {
        let delta = arguments.map(|text| {
            serde_json::json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "item_1",
                "delta": text,
            })
            .to_string()
        });
        let mut payloads = vec![
            r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"item_1","call_id":"call_abc","name":"glob"}}"#,
        ];
        payloads.extend(delta.as_deref());
        payloads.extend([
            r#"{"type":"response.function_call_arguments.done","item_id":"item_1"}"#,
            r#"{"type":"response.completed","response":{}}"#,
        ]);
        let shape = responses(&payloads).shape();
        assert!(
            shape.contains(&"tool.called:call_abc:glob:{}".to_owned()),
            "{arguments:?}: {shape:?}"
        );
    }
}

#[test]
fn responses_does_not_hide_an_incomplete_tool_response_as_continue() {
    let harness = responses(&[
        r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"item_1","call_id":"call_abc","name":"read_file"}}"#,
        r#"{"type":"response.function_call_arguments.delta","item_id":"item_1","delta":"{}"}"#,
        r#"{"type":"response.function_call_arguments.done","item_id":"item_1"}"#,
        r#"{"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"}}}"#,
    ]);
    assert_eq!(
        harness.shape(),
        vec![
            "tool.open:call_abc:read_file",
            "tool.delta:call_abc:{}",
            "tool.close:call_abc",
            "tool.called:call_abc:read_file:{}",
            "terminal:Length",
        ],
        "a tool call cannot erase the provider's explicit truncation"
    );
}

#[test]
fn responses_emits_tool_called_exactly_once_per_call() {
    // `output_item.done` arrives after `function_call_arguments.done` for the
    // same item. A decoder that treated both as "close" would emit a second
    // ToolCalled and the Step would execute the tool twice.
    let harness = responses(&[
        r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"item_1","call_id":"call_abc","name":"t"}}"#,
        r#"{"type":"response.function_call_arguments.delta","item_id":"item_1","delta":"{}"}"#,
        r#"{"type":"response.function_call_arguments.done","item_id":"item_1"}"#,
        r#"{"type":"response.output_item.done","item":{"id":"item_1"}}"#,
        r#"{"type":"response.completed","response":{}}"#,
    ]);
    let called = harness
        .fragments()
        .iter()
        .filter(|fragment| matches!(fragment, AttemptFragment::ToolCalled { .. }))
        .count();
    assert_eq!(called, 1, "a tool call must be emitted exactly once");
}

#[test]
fn responses_maps_the_incomplete_reasons_it_was_transcribed_from() {
    let length = responses(&[
        r#"{"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"}}}"#,
    ]);
    assert_eq!(length.shape(), vec!["terminal:Length"]);

    let filtered = responses(&[
        r#"{"type":"response.incomplete","response":{"incomplete_details":{"reason":"content_filter"}}}"#,
    ]);
    assert_eq!(filtered.shape(), vec!["terminal:ContentFilter"]);

    // A bare incomplete is a backend failure, never a successful completion.
    let bare = responses(&[r#"{"type":"response.incomplete","response":{}}"#]);
    assert_eq!(
        bare.terminal_class(),
        Some(&ProviderFailureClass::ProviderInternal { retry_after: None })
    );
}

#[test]
fn responses_refuses_a_function_call_missing_its_durable_identity() {
    // Dropping it would produce a Step that looks complete while the model was
    // asking for a tool — the exact failure the old blanket refusal existed to
    // prevent, preserved now that decoding exists.
    let harness = responses(&[
        r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"item_1","name":"t"}}"#,
    ]);
    assert_eq!(
        harness.terminal_class(),
        Some(&ProviderFailureClass::InvalidProviderOutput)
    );
}

const OPENED: &str = r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"item_1","call_id":"call_abc","name":"read_lines"}}"#;

#[test]
fn responses_takes_arguments_reported_only_when_the_call_finishes() {
    for (label, done) in [
        (
            "arguments.done",
            r#"{"type":"response.function_call_arguments.done","item_id":"item_1","arguments":"{\"path\":\"a.txt\"}"}"#,
        ),
        (
            "output_item.done",
            r#"{"type":"response.output_item.done","item":{"type":"function_call","id":"item_1","call_id":"call_abc","name":"read_lines","arguments":"{\"path\":\"a.txt\"}"}}"#,
        ),
    ] {
        let harness = responses(&[
            OPENED,
            done,
            r#"{"type":"response.completed","response":{}}"#,
        ]);
        assert_eq!(
            harness.shape(),
            vec![
                "tool.open:call_abc:read_lines",
                "tool.delta:call_abc:{\"path\":\"a.txt\"}",
                "tool.close:call_abc",
                "tool.called:call_abc:read_lines:{\"path\":\"a.txt\"}",
                "terminal:ToolCalls",
            ],
            "{label}: the reported arguments were dropped"
        );
    }
}

/// Streamed deltas with a matching final string, and a later item done that
/// repeats them, change nothing: one call, its input as streamed.
#[test]
fn responses_keeps_streamed_arguments_that_the_final_string_confirms() {
    let harness = responses(&[
        OPENED,
        r#"{"type":"response.function_call_arguments.delta","item_id":"item_1","delta":"{\"path\":"}"#,
        r#"{"type":"response.function_call_arguments.delta","item_id":"item_1","delta":"\"a.txt\"}"}"#,
        r#"{"type":"response.function_call_arguments.done","item_id":"item_1","arguments":"{\"path\":\"a.txt\"}"}"#,
        r#"{"type":"response.output_item.done","item":{"type":"function_call","id":"item_1","call_id":"call_abc","name":"read_lines","arguments":"{\"path\":\"a.txt\"}"}}"#,
        r#"{"type":"response.completed","response":{}}"#,
    ]);
    assert_eq!(
        harness.shape(),
        vec![
            "tool.open:call_abc:read_lines",
            "tool.delta:call_abc:{\"path\":",
            "tool.delta:call_abc:\"a.txt\"}",
            "tool.close:call_abc",
            "tool.called:call_abc:read_lines:{\"path\":\"a.txt\"}",
            "terminal:ToolCalls",
        ]
    );
}

/// Final arguments that disagree with the streamed ones fail the attempt:
/// neither version is run.
#[test]
fn responses_refuses_final_arguments_that_disagree_with_the_stream() {
    let harness = responses(&[
        OPENED,
        r#"{"type":"response.function_call_arguments.delta","item_id":"item_1","delta":"{\"path\":\"a.txt\"}"}"#,
        r#"{"type":"response.function_call_arguments.done","item_id":"item_1","arguments":"{\"path\":\"b.txt\"}"}"#,
        r#"{"type":"response.completed","response":{}}"#,
    ]);
    assert!(
        !harness
            .shape()
            .iter()
            .any(|step| step.starts_with("tool.called")),
        "{:?}",
        harness.shape()
    );
    assert_eq!(
        harness.terminal_class(),
        Some(&ProviderFailureClass::InvalidProviderOutput)
    );
}

/// Final-only arguments are bounded like streamed ones: over the bound, the
/// call is reported invalid and never run.
#[test]
fn responses_bounds_final_only_arguments() {
    let path = "a".repeat(crate::transport::sink::MAX_TOOL_INPUT_BYTES);
    let done = serde_json::json!({
        "type": "response.function_call_arguments.done",
        "item_id": "item_1",
        "arguments": format!("{{\"path\":\"{path}\"}}"),
    })
    .to_string();
    let harness = responses(&[
        OPENED,
        &done,
        r#"{"type":"response.completed","response":{}}"#,
    ]);
    let shape = harness.shape();
    assert!(
        !shape.iter().any(|step| step.starts_with("tool.called")),
        "{shape:?}"
    );
    assert!(
        harness
            .fragments()
            .iter()
            .any(|fragment| matches!(fragment, AttemptFragment::ToolInputInvalid { .. })),
        "{shape:?}"
    );
}
