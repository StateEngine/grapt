//! Signed and redacted replay boundary witnesses.
use super::*;
use crate::AdapterState;

#[test]
fn pc_anthropic_retains_signed_thinking_without_rendering_signature() {
    // `signature_delta` carries an opaque signature, not transcript text.
    // Rendering it would put base64 in the user's reasoning transcript.
    let harness = anthropic(&[
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"real"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"AAAAopaque"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    assert_eq!(
        harness.shape(),
        vec![
            "reason.open#0",
            "reason#0:real",
            "reason.close#0",
            "terminal:Unknown",
        ],
        "an opaque signature must neither reach the transcript nor silently disappear"
    );
    let state = harness
        .pending
        .iter()
        .find_map(|fragment| match fragment {
            AttemptFragment::ReasoningClosed { state, .. } => state.capture_bytes(),
            _ => None,
        })
        .unwrap_or_else(|| panic!("signed replay state missing"));
    let value: serde_json::Value =
        serde_json::from_slice(state).unwrap_or_else(|error| panic!("replay: {error}"));
    assert_eq!(value["thinking"], "real");
    assert_eq!(value["signature"], "AAAAopaque");
}

#[test]
fn pc_redacted_thinking_roundtrips_only_as_opaque_bound_content(
) -> Result<(), Box<dyn std::error::Error>> {
    let harness = anthropic(&[
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"PC_REDACTED"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    assert!(!format!("{:?}", harness.pending).contains("PC_REDACTED"));
    let state = harness
        .pending
        .iter()
        .find_map(|fragment| match fragment {
            AttemptFragment::ReasoningClosed { state, .. } => state.capture_bytes(),
            _ => None,
        })
        .ok_or("redacted state")?;
    let request = crate::transport::fixtures::anthropic_request();
    let model = request.model();
    let replay = crate::ProviderReplay::restore(
        model.provider().clone(),
        model.model().clone(),
        None,
        std::str::from_utf8(state)?,
    )?;
    assert_eq!(
        replay.anthropic_block(),
        Some(serde_json::json!({"type":"redacted_thinking", "data":"PC_REDACTED"}))
    );
    let missing = anthropic(&[
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking"}}"#,
    ]);
    assert_eq!(
        missing.terminal_class(),
        Some(&ProviderFailureClass::InvalidProviderOutput)
    );
    Ok(())
}

#[test]
fn pc_anthropic_partial_missing_oversized_and_overlapping_thinking_never_succeeds() {
    let start = r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}"#;
    let delta = r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"partial"}}"#;
    let stop = r#"{"type":"content_block_stop","index":0}"#;
    let terminal = r#"{"type":"message_stop"}"#;
    let huge = serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"x".repeat(AdapterState::MAX_BYTES + 1)}}).to_string();
    let overlap = r#"{"type":"content_block_start","index":1,"content_block":{"type":"thinking"}}"#;
    let early_tool = r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"early","name":"read","input":{}}}"#;
    for frames in [
        vec![start, delta, terminal],
        vec![start, delta, stop, terminal],
        vec![start, delta, &huge, stop, terminal],
        vec![start, delta, overlap, terminal],
        vec![start, delta, early_tool, terminal],
        vec![early_tool, start, terminal],
    ] {
        let harness = anthropic(&frames);
        assert_eq!(
            harness.terminal_class(),
            Some(&ProviderFailureClass::InvalidProviderOutput)
        );
        assert!(!harness
            .pending
            .iter()
            .any(|fragment| matches!(fragment, AttemptFragment::ProviderTerminal { .. })));
        assert!(!harness
            .pending
            .iter()
            .any(|fragment| matches!(fragment, AttemptFragment::ToolCalled { .. })));
    }
}

fn bound_responses(payloads: &[&str], binding: Option<crate::replay::ReplayBinding>) -> Harness {
    let mut harness = Harness::new();
    let mut decoder = ResponsesDecoder::for_route(UsageRoute::ChatGptCodex).replaying(binding);
    for payload in payloads {
        let mut sink = harness.sink();
        decoder.decode(payload, &mut sink);
    }
    harness
}

const REASONING_THEN_CALL: [&str; 8] = [
    r#"{"type":"response.output_item.added","item":{"type":"reasoning","id":"rs_pc_1","summary":[]}}"#,
    r#"{"type":"response.reasoning_summary_text.delta","item_id":"rs_pc_1","delta":"plan"}"#,
    r#"{"type":"response.reasoning_summary_text.done","item_id":"rs_pc_1"}"#,
    r#"{"type":"response.output_item.done","item":{"type":"reasoning","id":"rs_pc_1","status":"completed","summary":[{"type":"summary_text","text":"plan"}],"encrypted_content":"PC_ENCRYPTED"}}"#,
    r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"fc_pc_1","call_id":"call_pc_1","name":"read"}}"#,
    r#"{"type":"response.function_call_arguments.delta","item_id":"fc_pc_1","delta":"{}"}"#,
    r#"{"type":"response.function_call_arguments.done","item_id":"fc_pc_1"}"#,
    r#"{"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}"#,
];

#[test]
fn pc_responses_reasoning_is_retained_and_replayed_verbatim_before_its_call(
) -> Result<(), Box<dyn std::error::Error>> {
    let harness = bound_responses(
        &REASONING_THEN_CALL,
        Some(crate::replay::ReplayBinding::fixture()),
    );
    assert!(harness.terminal_class().is_none(), "{:?}", harness.shape());
    assert!(
        !harness
            .shape()
            .iter()
            .any(|row| row.contains("PC_ENCRYPTED")),
        "encrypted content must never reach the transcript"
    );
    let states: Vec<&[u8]> = harness
        .pending
        .iter()
        .filter_map(|fragment| match fragment {
            AttemptFragment::ReasoningClosed { state, .. } => state.capture_bytes(),
            _ => None,
        })
        .collect();
    assert_eq!(states.len(), 1, "one completed reasoning item, one replay");
    let model = crate::transport::fixtures::responses_request()
        .model()
        .clone();
    let replay = crate::ProviderReplay::restore(
        model.provider().clone(),
        model.model().clone(),
        None,
        std::str::from_utf8(states[0])?,
    )?;
    let call = ToolCallId::new("call_pc_1").ok_or("call")?;
    let request = crate::ChatGptCodexRequest {
        model,
        input: vec![
            crate::transport::fixtures::user("read it").into(),
            crate::CodexInputItem::ProviderReplay(replay),
            crate::CodexInputItem::ToolCall {
                call: call.clone(),
                tool: crate::ToolName::new("read").ok_or("tool")?,
                arguments: crate::CodexToolText::try_new("{}".into())
                    .map_err(|error| error.to_string())?,
            },
            crate::CodexInputItem::ToolOutput {
                call,
                output: crate::CodexToolText::try_new("contents".into())
                    .map_err(|error| error.to_string())?,
                is_error: false,
            },
        ],
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
        reasoning: Default::default(),
        turn: Default::default(),
    };
    let body = crate::routes::body::codex_body(&request)?;
    assert_eq!(
        body["include"],
        serde_json::json!(["reasoning.encrypted_content"])
    );
    assert_eq!(
        body["input"][1],
        serde_json::json!({
            "type": "reasoning",
            "id": "rs_pc_1",
            "summary": [{"type": "summary_text", "text": "plan"}],
            "encrypted_content": "PC_ENCRYPTED",
        })
    );
    assert_eq!(body["input"][2]["type"], "function_call");
    Ok(())
}

#[test]
fn pc_responses_reasoning_without_a_binding_or_past_its_bound_is_dropped_not_failed() {
    let mut oversized = REASONING_THEN_CALL;
    let big = format!(
        r#"{{"type":"response.output_item.done","item":{{"type":"reasoning","id":"rs_pc_1","summary":[],"encrypted_content":"{}"}}}}"#,
        "e".repeat(crate::replay::MAX_REASONING_ITEM_BYTES)
    );
    oversized[3] = &big;
    for (label, harness) in [
        ("unbound", bound_responses(&REASONING_THEN_CALL, None)),
        (
            "oversized",
            bound_responses(&oversized, Some(crate::replay::ReplayBinding::fixture())),
        ),
    ] {
        assert!(
            harness.terminal_class().is_none(),
            "{label}: {:?}",
            harness.shape()
        );
        assert!(
            harness.pending.iter().all(|fragment| !matches!(
                fragment,
                AttemptFragment::ReasoningClosed { state, .. } if state.is_present()
            )),
            "{label}: no replay state"
        );
        assert!(
            harness
                .shape()
                .iter()
                .any(|row| row.starts_with("tool.called:call_pc_1")),
            "{label}: the call still completes"
        );
    }
}

/// A compaction_trigger response's compaction item is retained whole under
/// the compaction binding, and goes back verbatim as input.
#[test]
fn pc_responses_compaction_item_is_retained_and_replayed_verbatim(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut harness = Harness::new();
    let mut decoder = ResponsesDecoder::for_route(UsageRoute::ChatGptCodex)
        .compacting(Some(crate::replay::ReplayBinding::fixture()));
    for payload in [
        r#"{"type":"response.output_item.done","item":{"type":"compaction","id":"cmp_pc_1","encrypted_content":"PC_COMPACTED"}}"#,
        r#"{"type":"response.completed","response":{"usage":{"input_tokens":9,"output_tokens":2,"total_tokens":11}}}"#,
    ] {
        let mut sink = harness.sink();
        decoder.decode(payload, &mut sink);
    }
    assert!(harness.terminal_class().is_none(), "{:?}", harness.shape());
    assert!(!harness
        .shape()
        .iter()
        .any(|row| row.contains("PC_COMPACTED")));
    let state = harness
        .pending
        .iter()
        .find_map(|fragment| match fragment {
            AttemptFragment::ReasoningClosed { state, .. } => state.capture_bytes(),
            _ => None,
        })
        .ok_or("compaction state")?;
    let model = crate::transport::fixtures::responses_request()
        .model()
        .clone();
    let replay = crate::ProviderReplay::restore(
        model.provider().clone(),
        model.model().clone(),
        None,
        std::str::from_utf8(state)?,
    )?;
    assert!(replay.is_responses_compaction());
    let request = crate::ChatGptCodexRequest {
        model,
        input: vec![crate::CodexInputItem::ProviderReplay(replay)],
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
        reasoning: Default::default(),
        turn: Default::default(),
    };
    assert_eq!(
        crate::routes::body::codex_body(&request)?["input"][0],
        serde_json::json!({"type":"compaction","id":"cmp_pc_1","encrypted_content":"PC_COMPACTED"})
    );
    Ok(())
}

/// In a compaction_trigger response, an output item the decoder
/// would drop (reasoning without encrypted content, an empty message) still
/// reaches the call as an empty stateless reasoning block, so the call can
/// count it. Outside a compaction call nothing is emitted for them.
#[test]
fn pc_dropped_items_reach_only_a_compaction_call() {
    for triggered in [true, false] {
        let mut harness = Harness::new();
        let mut decoder = ResponsesDecoder::for_route(UsageRoute::ChatGptCodex)
            .compacting(Some(crate::replay::ReplayBinding::fixture()))
            .triggered(triggered);
        for payload in [
            r#"{"type":"response.output_item.done","item":{"type":"reasoning","id":"rs_extra","summary":[]}}"#,
            r#"{"type":"response.output_item.done","item":{"type":"message","id":"msg_extra","role":"assistant","status":"completed","content":[]}}"#,
            r#"{"type":"response.output_item.done","item":{"type":"compaction","id":"cmp_pc_1","encrypted_content":"PC_COMPACTED"}}"#,
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":9,"output_tokens":2,"total_tokens":11}}}"#,
        ] {
            let mut sink = harness.sink();
            decoder.decode(payload, &mut sink);
        }
        assert!(harness.terminal_class().is_none(), "{:?}", harness.shape());
        let closed: Vec<bool> = harness
            .pending
            .iter()
            .filter_map(|fragment| match fragment {
                AttemptFragment::ReasoningClosed { state, .. } => {
                    Some(state.capture_bytes().is_some())
                }
                _ => None,
            })
            .collect();
        let expected = if triggered {
            vec![false, false, true]
        } else {
            vec![true]
        };
        assert_eq!(closed, expected, "triggered={triggered}");
    }
}
