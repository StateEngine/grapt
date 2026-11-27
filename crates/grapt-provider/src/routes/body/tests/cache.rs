//! Anthropic cache-marker placement on the wire.
//!
//! Markers are requests for provider reuse. These tests prove where they land
//! and that the bytes they close stay stable as a conversation grows; they do
//! not, and cannot, prove a provider cache hit.

use super::{body, bound, keys, model, system, tool, user};
use crate::request::{
    AnthropicMessagesRequest, AnthropicPromptCache, CodexInputItem, ProviderRequest, ToolDefinition,
};

const MARKER: &str = "cache_control";

fn call(id: &str, path: &str) -> [CodexInputItem; 2] {
    let text = |value: String| {
        crate::CodexToolText::try_new(value).unwrap_or_else(|error| panic!("text: {error}"))
    };
    let call = grapt_types::ToolCallId::new(id).unwrap_or_else(|| panic!("call"));
    [
        CodexInputItem::ToolCall {
            call: call.clone(),
            tool: crate::ToolName::new("read_file").unwrap_or_else(|| panic!("tool")),
            arguments: text(format!(r#"{{"path":"{path}"}}"#)),
        },
        CodexInputItem::ToolOutput {
            call,
            output: text(format!("contents of {path}")),
            is_error: false,
        },
    ]
}

fn second_tool() -> ToolDefinition {
    ToolDefinition {
        input_format: crate::ToolInputFormat::Json,
        name: "write_file".to_owned(),
        description: None,
        input_schema: r#"{"type":"object"}"#.to_owned(),
    }
}

fn request(messages: Vec<CodexInputItem>, prompt_cache: AnthropicPromptCache) -> ProviderRequest {
    ProviderRequest::AnthropicMessages(AnthropicMessagesRequest {
        web_search: None,
        uncached_system_suffix: None,
        effort: crate::EffortPolicy::default(),
        model: model("anthropic", "claude-opus-5"),
        messages,
        max_tokens: bound(8192),
        instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
        thinking: None,
        prompt_cache,
        tools: vec![tool(), second_tool()],
        tool_choice: None,
    })
}

fn conversation(reads: &[(&str, &str)]) -> Vec<CodexInputItem> {
    let mut messages = vec![
        system("base instructions").into(),
        system("project instructions").into(),
        user("inspect the repository").into(),
    ];
    for (id, path) in reads {
        messages.extend(call(id, path));
    }
    messages
}

/// The body with every marker removed: what the provider hashes as content.
fn unmarked(mut value: serde_json::Value) -> serde_json::Value {
    match &mut value {
        serde_json::Value::Object(object) => {
            object.remove("cache_control");
            for child in object.values_mut() {
                *child = unmarked(child.take());
            }
        }
        serde_json::Value::Array(items) => {
            for child in items.iter_mut() {
                *child = unmarked(child.take());
            }
        }
        _ => {}
    }
    value
}

#[test]
fn cq_stable_prefix_marks_tools_system_previous_input_and_tail() {
    let cached = request(
        conversation(&[("call_a", "a.rs")]),
        AnthropicPromptCache::StablePrefix,
    );
    let emitted = body(&cached);
    let text = emitted.to_string();
    assert_eq!(text.matches(MARKER).count(), 4, "{text}");

    assert!(emitted["tools"][0].get("cache_control").is_none());
    assert_eq!(
        emitted["tools"][1]["cache_control"],
        serde_json::json!({"type":"ephemeral","ttl":"1h"})
    );
    assert_eq!(
        emitted["system"],
        serde_json::json!([{
            "type": "text",
            "text": "base instructions\n\nproject instructions",
            "cache_control": {"type": "ephemeral", "ttl":"1h"},
        }])
    );
    let messages = emitted["messages"].as_array().cloned().unwrap_or_default();
    assert_eq!(messages.len(), 3);
    let tail = &messages[2]["content"][0];
    assert_eq!(tail["type"], "tool_result");
    assert_eq!(tail["cache_control"]["type"], "ephemeral");
    assert_eq!(
        messages[0]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    assert!(messages[1]["content"][0].get("cache_control").is_none());

    // Caching adds no top-level key and changes no field other than the
    // system encoding and the four markers.
    let off = request(
        conversation(&[("call_a", "a.rs")]),
        AnthropicPromptCache::Off,
    );
    assert_eq!(keys(&cached), keys(&off));
    let mut expected = body(&off);
    expected["system"] = serde_json::json!([{
        "type": "text",
        "text": "base instructions\n\nproject instructions",
    }]);
    assert_eq!(unmarked(emitted), expected);
}

#[test]
fn cq_off_emits_no_marker_and_keeps_the_system_string() {
    let emitted = body(&request(
        conversation(&[("call_a", "a.rs")]),
        AnthropicPromptCache::Off,
    ));
    assert!(!emitted.to_string().contains("cache_control"));
    assert_eq!(
        emitted["system"],
        "base instructions\n\nproject instructions"
    );
}

#[test]
fn cq_cached_prefix_is_byte_stable_while_tool_history_grows() {
    let first = body(&request(
        conversation(&[("call_a", "a.rs")]),
        AnthropicPromptCache::StablePrefix,
    ));
    let second = body(&request(
        conversation(&[("call_a", "a.rs"), ("call_b", "b.rs")]),
        AnthropicPromptCache::StablePrefix,
    ));
    // The tools and system markers close identical bytes in both requests.
    assert_eq!(first["tools"].to_string(), second["tools"].to_string());
    assert_eq!(first["system"].to_string(), second["system"].to_string());
    // Every earlier message is unchanged apart from the moved tail marker, so
    // the first request's cached prefix is a prefix of the second request.
    let before = unmarked(first["messages"].clone());
    let after = unmarked(second["messages"].clone());
    let before = before.as_array().cloned().unwrap_or_default();
    let after = after.as_array().cloned().unwrap_or_default();
    assert_eq!(before.len(), 3);
    assert_eq!(after.len(), 5);
    assert_eq!(after[..3], before[..]);
    assert_eq!(second.to_string().matches(MARKER).count(), 4);
    assert_eq!(second["messages"][4]["content"][0]["type"], "tool_result");
    assert_eq!(
        second["messages"][4]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    // A retry serializes the identical request to identical bytes.
    let retry = body(&request(
        conversation(&[("call_a", "a.rs"), ("call_b", "b.rs")]),
        AnthropicPromptCache::StablePrefix,
    ));
    assert_eq!(second.to_string(), retry.to_string());
}

#[test]
fn cq_changed_model_or_tools_is_visible_in_the_closed_prefix() {
    let base = body(&request(
        conversation(&[("call_a", "a.rs")]),
        AnthropicPromptCache::StablePrefix,
    ));
    let ProviderRequest::AnthropicMessages(mut changed) = request(
        conversation(&[("call_a", "a.rs")]),
        AnthropicPromptCache::StablePrefix,
    ) else {
        panic!("family");
    };
    changed.model = model("anthropic", "claude-sonnet-5");
    let other_model = body(&ProviderRequest::AnthropicMessages(changed.clone()));
    assert_ne!(base["model"], other_model["model"]);
    changed.tools.pop();
    let fewer_tools = body(&ProviderRequest::AnthropicMessages(changed));
    assert_ne!(base["tools"].to_string(), fewer_tools["tools"].to_string());
    assert_eq!(fewer_tools["tools"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        fewer_tools["tools"][0]["cache_control"],
        serde_json::json!({"type": "ephemeral", "ttl": "1h"})
    );
    assert_eq!(
        fewer_tools["system"][0]["cache_control"]["type"],
        "ephemeral"
    );
}

#[test]
fn cq_tail_marker_skips_blocks_that_cannot_carry_it() {
    let mut messages = vec![
        serde_json::json!({"role": "user", "content": [{"type": "text", "text": "inspect"}]}),
        serde_json::json!({"role": "assistant", "content": [
            {"type": "text", "text": "reading"},
            {"type": "thinking", "thinking": "t", "signature": "s"},
            {"type": "redacted_thinking", "data": "d"},
            {"type": "text", "text": ""},
        ]}),
    ];
    super::super::mark_conversation_breakpoints(&mut messages);
    assert_eq!(
        messages[1]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    let skipped = messages[1]["content"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(skipped.len(), 4);
    assert!(skipped
        .iter()
        .skip(1)
        .all(|block| block.get("cache_control").is_none()));
    assert!(messages[0]["content"][0].get("cache_control").is_none());

    let mut nothing = vec![serde_json::json!({"role": "assistant", "content": [
        {"type": "redacted_thinking", "data": "d"},
    ]})];
    super::super::mark_conversation_breakpoints(&mut nothing);
    assert!(!serde_json::Value::Array(nothing)
        .to_string()
        .contains("cache_control"));
}

#[test]
fn cq_no_tools_and_no_system_leave_only_the_tail_marker() {
    let ProviderRequest::AnthropicMessages(mut bare) = request(
        vec![user("hello").into()],
        AnthropicPromptCache::StablePrefix,
    ) else {
        panic!("family");
    };
    bare.tools.clear();
    let emitted = body(&ProviderRequest::AnthropicMessages(bare));
    assert!(emitted.get("tools").is_none());
    assert!(emitted.get("system").is_none());
    assert_eq!(emitted.to_string().matches(MARKER).count(), 1);
    assert_eq!(
        emitted["messages"][0]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
}

#[test]
fn cq_previous_input_anchor_survives_a_parallel_burst_beyond_the_lookback_span() {
    let first = body(&request(
        conversation(&[]),
        AnthropicPromptCache::StablePrefix,
    ));
    assert_eq!(
        first["tools"][1]["cache_control"]["type"], "ephemeral",
        "the final tool is always marked"
    );
    let mut messages = conversation(&[]);
    let mut results = Vec::new();
    for ordinal in 0..32 {
        let [invoked, result] = call(&format!("parallel_{ordinal}"), "module.rs");
        messages.push(invoked);
        results.push(result);
    }
    messages.extend(results);
    let next = body(&request(messages, AnthropicPromptCache::StablePrefix));
    assert_eq!(
        next["messages"][1]["content"].as_array().map(Vec::len),
        Some(32)
    );
    assert_eq!(
        next["messages"][2]["content"].as_array().map(Vec::len),
        Some(32)
    );
    assert_eq!(
        first["messages"][0], next["messages"][0],
        "the old marked input survives beyond 20 new blocks"
    );
    assert_eq!(
        next["messages"][2]["content"][31]["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(
        next.to_string().matches(MARKER).count(),
        4,
        "native caching uses the four supported slots"
    );
    assert_eq!(
        unmarked(first["tools"].clone()),
        unmarked(next["tools"].clone())
    );
    assert_eq!(first["system"], next["system"]);
}

#[test]
fn cq_previous_anchor_skips_all_trailing_user_blocks_and_uses_only_available_slots() {
    let mut messages = vec![
        serde_json::json!({"role":"user","content":[{"type":"text","text":"old input"}]}),
        serde_json::json!({"role":"assistant","content":[{"type":"text","text":"reply"}]}),
        serde_json::json!({"role":"user","content":[{"type":"text","text":"new task"}]}),
        serde_json::json!({"role":"user","content":[{"type":"text","text":"mail"}]}),
    ];
    assert_eq!(
        super::super::mark_conversation_breakpoints(&mut messages),
        2
    );
    assert_eq!(
        messages[0]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    assert!(messages[2]["content"][0].get("cache_control").is_none());
    assert_eq!(
        messages[3]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
}

#[test]
fn cq_previous_anchor_stays_within_budget_when_static_sections_are_absent() {
    for (include_system, include_tools, expected) in
        [(false, true, 3), (true, false, 3), (false, false, 2)]
    {
        let ProviderRequest::AnthropicMessages(mut message) = request(
            conversation(&[("one", "one.rs")]),
            AnthropicPromptCache::StablePrefix,
        ) else {
            panic!("family")
        };
        if !include_system {
            message.messages.retain(|item| !matches!(item, CodexInputItem::Message(message) if message.role == crate::MessageRole::System));
        }
        if !include_tools {
            message.tools.clear();
        }
        let emitted = body(&ProviderRequest::AnthropicMessages(message));
        assert_eq!(emitted.to_string().matches(MARKER).count(), expected);
        assert_eq!(
            emitted["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
    }
}

#[test]
fn a_previous_system_input_remains_the_native_cache_anchor() {
    let mut input = vec![
        system("baseline").into(),
        user("question").into(),
        system("policy update").into(),
    ];
    let before = body(&request(input.clone(), AnthropicPromptCache::StablePrefix));
    input.push(
        crate::InputMessage {
            role: crate::MessageRole::Assistant,
            content: "answer".into(),
        }
        .into(),
    );
    input.push(user("continue").into());
    let after = body(&request(input.clone(), AnthropicPromptCache::StablePrefix));
    assert_eq!(before["system"], after["system"]);
    assert_eq!(after["messages"][1]["role"], "system");
    assert_eq!(
        after["messages"][1]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    let prior = unmarked(before["messages"].clone());
    let next = unmarked(after["messages"].clone());
    let prior = prior.as_array().unwrap_or_else(|| panic!("prior messages"));
    assert_eq!(
        &next.as_array().unwrap_or_else(|| panic!("next messages"))[..prior.len()],
        prior
    );
    assert_eq!(after.to_string().matches(MARKER).count(), 4);

    let mut unsupported = request(input, AnthropicPromptCache::StablePrefix);
    let ProviderRequest::AnthropicMessages(ref mut value) = unsupported else {
        panic!("family")
    };
    value.model = model("anthropic", "claude-sonnet-5");
    assert!(crate::routes::body::anthropic_body(value).is_err());
}

#[test]
fn cache04_session_system_context_follows_a_shared_hour_long_prefix() {
    let make = |session: &str, cached| {
        let ProviderRequest::AnthropicMessages(mut request) =
            request(conversation(&[("one", "file.rs")]), cached)
        else {
            panic!("family")
        };
        request.uncached_system_suffix = Some(format!("Session ID: {session}"));
        ProviderRequest::AnthropicMessages(request)
    };
    let a = make("session-a", AnthropicPromptCache::StablePrefix);
    let b = make("session-b", AnthropicPromptCache::StablePrefix);
    let before = body(&a);
    let after = body(&b);
    assert_eq!(before["tools"], after["tools"]);
    assert_eq!(before["system"][0], after["system"][0]);
    assert_eq!(
        before["system"][0]["cache_control"],
        serde_json::json!({"type":"ephemeral","ttl":"1h"})
    );
    assert_eq!(
        before["system"][1],
        serde_json::json!({"type":"text","text":"Session ID: session-a"})
    );
    assert_eq!(
        after["system"][1],
        serde_json::json!({"type":"text","text":"Session ID: session-b"})
    );
    assert!(!before["messages"].to_string().contains("session-a"));
    assert_eq!(before.to_string().matches("cache_control").count(), 4);
    assert_eq!(before["tools"][1]["cache_control"]["ttl"], "1h");
    assert_ne!(
        crate::request::prefix_digest(&a, 0),
        crate::request::prefix_digest(&b, 0)
    );
    let off = body(&make("session-a", AnthropicPromptCache::Off));
    assert_eq!(
        off["system"],
        "base instructions\n\nproject instructions\n\nSession ID: session-a"
    );
    assert!(!off.to_string().contains("cache_control"));
}
