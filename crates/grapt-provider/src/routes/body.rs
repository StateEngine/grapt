//! Request-body serialization, one builder per protocol family.
//!
//! Every body is built explicitly, one key at a time. See the module note in
//! [`crate::routes`] for why there is no derive, no bag and no overlay.
//!
//! # The regression these builders exist to make impossible
//!
//! The oracle builds one generic body and then *removes* fields the route
//! cannot accept (the oracle's `openai-responses.ts:122`), guarded
//! only by a test asserting absence. The Rust answer is two-layered: the field
//! does not exist on the route's request type (compile-fail probe 04), and the
//! builder below emits a key set that is asserted directly. Either layer alone
//! is weaker — a type cannot stop a serializer inserting a key by string, and a
//! key-set assertion cannot stop a caller setting a field.

use crate::request::{
    AnthropicMessagesRequest, AnthropicPromptCache, ChatGptCodexRequest, CodexInputItem,
    CompatiblePromptCache, MessageRole, OpenAiCompatibleRequest, OpenAiResponsesRequest,
    ToolChoice, ToolDefinition, ToolInputFormat,
};

/// A tool's JSON-Schema text, parsed.
///
/// Staging rejects malformed or non-object JSON before invoking a builder.
/// This private fallback cannot reach transport. Schema semantics remain the
/// selected provider's responsibility.
fn schema_of(tool: &ToolDefinition) -> serde_json::Value {
    serde_json::from_str::<serde_json::Value>(&tool.input_schema).unwrap_or(serde_json::Value::Null)
}

fn insert_description(
    body: &mut serde_json::Map<String, serde_json::Value>,
    tool: &ToolDefinition,
) {
    if let Some(description) = &tool.description {
        body.insert(
            "description".to_owned(),
            serde_json::Value::String(description.clone()),
        );
    }
}

fn role_str(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
    }
}

fn codex_input_array(
    input: &[CodexInputItem],
    effort: Option<&crate::effort::Projection>,
) -> Result<Vec<serde_json::Value>, &'static str> {
    let mut established = effort.and_then(|effort| effort.baseline);
    let text_calls = input
        .iter()
        .filter_map(|item| match item {
            CodexInputItem::TextToolCall { call, .. } => Some(call),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    input
        .iter()
        .filter(|item| !matches!(item, CodexInputItem::AssistantResponseStart))
        .filter(|item| match item {
            CodexInputItem::ProviderReplay(replay) if replay.is_responses_compaction() => {
                // Explicit compaction retires the visible override history.
                // Reassert the desired effort before the next retained User.
                established = None;
                true
            }
            CodexInputItem::EffortUpdate(value) => {
                let emit =
                    effort.is_some_and(|effort| effort.updates) && established != Some(*value);
                established = Some(*value);
                emit
            }
            _ => true,
        })
        .map(|item| {
            Ok(match item {
                CodexInputItem::CompatibleReasoning(_) => {
                    return Err("compatible reasoning requires its declared protocol")
                }
                CodexInputItem::AssistantResponseStart => {
                    return Err("unexpected response boundary")
                }
                // A reasoning item goes back exactly as the route produced it.
                CodexInputItem::ProviderReplay(replay) => replay
                    .responses_item()
                    .ok_or("signed replay requires its original protocol")?
                    .clone(),
                CodexInputItem::EffortUpdate(effort) => serde_json::json!({
                    "type":"configuration_update","reasoning":{"effort":effort.as_str()}
                }),
                CodexInputItem::Message(message) => serde_json::json!({
                    "role": role_str(message.role),
                    "content": message.content,
                }),
                CodexInputItem::ImageUser { text, images } => serde_json::json!({
                    "role":"user", "content":super::image_content::responses(text.as_str(), images),
                }),
                CodexInputItem::ImageToolOutput { call, output, images } => serde_json::json!({
                    "type":if text_calls.contains(call) { "custom_tool_call_output" } else { "function_call_output" }, "call_id":call.as_str(),
                    "output":super::image_content::responses(output.as_str(), images),
                }),
                CodexInputItem::ToolCall {
                    call,
                    tool,
                    arguments,
                } => serde_json::json!({
                    "type": "function_call",
                    "call_id": call.as_str(),
                    "name": tool.as_str(),
                    "arguments": arguments.object_text(),
                }),
                CodexInputItem::TextToolCall { call, tool, input } => serde_json::json!({
                    "type":"custom_tool_call", "call_id":call.as_str(),
                    "name":tool.as_str(), "input":input.as_str(),
                }),
                CodexInputItem::CompactionTrigger => {
                    serde_json::json!({"type": "compaction_trigger"})
                }
                CodexInputItem::ToolOutput { call, output, .. } => serde_json::json!({
                    "type": if text_calls.contains(call) { "custom_tool_call_output" } else { "function_call_output" },
                    "call_id": call.as_str(),
                    "output": output.as_str(),
                }),
            })
        })
        .collect::<Result<Vec<serde_json::Value>, &'static str>>()
        .map(|items| {
            let mut result: Vec<serde_json::Value> = Vec::with_capacity(items.len());
            for item in items {
                if item["type"] == "configuration_update"
                    && result
                        .last()
                        .is_some_and(|last| last["type"] == "configuration_update")
                {
                    result.pop();
                }
                result.push(item);
            }
            result
        })
}

// ---------------------------------------------------------------------------
// OpenAI Responses
// ---------------------------------------------------------------------------

/// The Responses tool shape: a flat `{type, name, parameters}`.
fn responses_tools(tools: &[ToolDefinition]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|tool| {
            let mut body = serde_json::Map::new();
            body.insert("name".to_owned(), serde_json::json!(tool.name));
            match &tool.input_format {
                ToolInputFormat::Json => {
                    body.insert("type".to_owned(), serde_json::json!("function"));
                    body.insert("parameters".to_owned(), schema_of(tool));
                }
                ToolInputFormat::Text { grammar } => {
                    body.insert("type".to_owned(), serde_json::json!("custom"));
                    body.insert(
                        "format".to_owned(),
                        serde_json::json!({
                            "type":"grammar", "syntax":"lark", "definition":grammar,
                        }),
                    );
                }
            }
            insert_description(&mut body, tool);
            serde_json::Value::Object(body)
        })
        .collect()
}

pub(crate) fn responses_tool_choice(choice: &ToolChoice) -> serde_json::Value {
    match choice {
        ToolChoice::Auto => serde_json::Value::String("auto".to_owned()),
        ToolChoice::None => serde_json::Value::String("none".to_owned()),
        ToolChoice::Required => serde_json::Value::String("required".to_owned()),
        ToolChoice::Named { tool } => serde_json::json!({ "type": "function", "name": tool }),
    }
}

fn responses_choice_with_tools(choice: &ToolChoice, tools: &[ToolDefinition]) -> serde_json::Value {
    if let ToolChoice::Named { tool } = choice {
        if tools.iter().any(|definition| {
            definition.name == *tool
                && matches!(definition.input_format, ToolInputFormat::Text { .. })
        }) {
            return serde_json::json!({"type":"custom", "name":tool});
        }
    }
    responses_tool_choice(choice)
}

/// The public OpenAI Responses body.
///
/// Carries `max_output_tokens` — the field is not banned globally, it is banned
/// on the route that rejects it. A blanket removal would be a different, wrong
/// fix.
pub(crate) fn openai_responses_body(
    request: &OpenAiResponsesRequest,
) -> Result<serde_json::Value, &'static str> {
    openai_responses_body_with_model(request, request.model.model())
}

pub(crate) fn openai_responses_body_with_model(
    request: &OpenAiResponsesRequest,
    wire_model: &grapt_types::ModelId,
) -> Result<serde_json::Value, &'static str> {
    let mut body = serde_json::Map::new();
    body.insert(
        "model".to_owned(),
        serde_json::Value::String(wire_model.as_str().to_owned()),
    );
    if let Some(effort) = request.model.variant() {
        body.insert(
            "reasoning".to_owned(),
            serde_json::json!({"effort": effort.as_str()}),
        );
    }
    body.insert(
        "input".to_owned(),
        serde_json::Value::Array(codex_input_array(&request.input, None)?),
    );
    if let Some(key) = request.prompt_cache_key.as_ref() {
        body.insert(
            "prompt_cache_key".to_owned(),
            serde_json::Value::String(key.as_str().to_owned()),
        );
    }
    body.insert("stream".to_owned(), serde_json::Value::Bool(true));
    if let Some(bound) = request.max_output_tokens {
        body.insert(
            "max_output_tokens".to_owned(),
            serde_json::Value::from(bound.value()),
        );
    }
    let mut tools = responses_tools(&request.tools);
    if let Some(search) = &request.web_search {
        // Native tools keep a fixed prefix as deferred function tools activate.
        tools.insert(0, search.wire());
        body.insert(
            "include".to_owned(),
            serde_json::json!(["web_search_call.action.sources"]),
        );
    }
    if !tools.is_empty() {
        body.insert("tools".to_owned(), serde_json::Value::Array(tools));
    }
    if let Some(choice) = request.tool_choice.as_ref() {
        body.insert(
            "tool_choice".to_owned(),
            responses_choice_with_tools(choice, &request.tools),
        );
    }
    Ok(serde_json::Value::Object(body))
}

/// The ChatGPT/Codex body.
///
/// `max_output_tokens` is unnameable here because [`ChatGptCodexRequest`] has
/// no such field. This function is the second half of that law.
pub(crate) fn codex_body(request: &ChatGptCodexRequest) -> Result<serde_json::Value, &'static str> {
    let mut body = serde_json::Map::new();
    let mut reasoning = serde_json::Map::new();
    let effort =
        crate::effort::projection(&request.input, &request.model, request.reasoning.effort);
    if let Some(value) = effort.baseline {
        reasoning.insert("effort".to_owned(), serde_json::json!(value.as_str()));
    } else if let Some(value) = request.model.variant() {
        reasoning.insert("effort".to_owned(), serde_json::json!(value.as_str()));
    }
    // Codex 0.155.1 `build_reasoning`: a Responses Lite model keeps all turns'
    // reasoning; otherwise the backend default keeps only the current turn's,
    // so earlier turns' replayed reasoning would leave the rendered prefix.
    if request.reasoning.all_turns {
        reasoning.insert("context".to_owned(), serde_json::json!("all_turns"));
    }
    if request.reasoning.summary {
        reasoning.insert("summary".to_owned(), serde_json::json!("auto"));
    }
    if !reasoning.is_empty() {
        body.insert("reasoning".to_owned(), serde_json::Value::Object(reasoning));
    }
    body.insert(
        "model".to_owned(),
        serde_json::Value::String(request.model.model().as_str().to_owned()),
    );
    body.insert(
        "input".to_owned(),
        serde_json::Value::Array(codex_input_array(&request.input, Some(&effort))?),
    );
    if let Some(key) = request.prompt_cache_key.as_ref() {
        body.insert(
            "prompt_cache_key".to_owned(),
            serde_json::Value::String(key.as_str().to_owned()),
        );
    }
    // ChatGPT-plan requests are ephemeral at the provider boundary. The
    // frozen oracle supplies this as the OpenAI Responses route default; D0's
    // first real Codex request proved omission is rejected as invalid input.
    body.insert("store".to_owned(), serde_json::Value::Bool(false));
    // Without storage the route can continue a reasoning model's thought only
    // from the reasoning items the request carries back, so it is asked for
    // their encrypted content, as the oracle asks.
    let mut include = vec!["reasoning.encrypted_content"];
    if request.web_search.is_some() {
        include.push("web_search_call.action.sources");
    }
    body.insert("include".to_owned(), serde_json::json!(include));
    body.insert("stream".to_owned(), serde_json::Value::Bool(true));
    let mut tools = responses_tools(&request.tools);
    if let Some(search) = &request.web_search {
        // Native tools keep a fixed prefix as deferred function tools activate.
        tools.insert(0, search.wire());
    }
    if !tools.is_empty() {
        body.insert("tools".to_owned(), serde_json::Value::Array(tools));
    }
    if let Some(choice) = request.tool_choice.as_ref() {
        body.insert(
            "tool_choice".to_owned(),
            responses_choice_with_tools(choice, &request.tools),
        );
    }
    Ok(serde_json::Value::Object(body))
}

// ---------------------------------------------------------------------------
// Anthropic Messages
// ---------------------------------------------------------------------------

/// The Anthropic body.
///
/// Three shape differences from the OpenAI routes, each a protocol fact rather
/// than a preference:
///
/// - `max_tokens` is **required**, which is why the request type carries it as
///   a value rather than an `Option`;
/// - initial System instructions form the top-level `system` field; supported
///   native models keep subsequent System updates in chronological messages;
/// - tools carry `input_schema`, not `parameters`.
///
/// Native caching uses one-hour markers for the stable system prefix, both
/// conversation anchors and the last tool, within Anthropic's four slots.
/// Session-specific System context follows the stable system boundary.
/// Compatible gateways retain their independent policy below.
pub(crate) fn anthropic_body(
    request: &AnthropicMessagesRequest,
) -> Result<serde_json::Value, &'static str> {
    anthropic_body_with_model(request, request.model.model())
}

pub(crate) fn anthropic_body_with_model(
    request: &AnthropicMessagesRequest,
    wire_model: &grapt_types::ModelId,
) -> Result<serde_json::Value, &'static str> {
    let mut body = anthropic_parts(request)?;
    body.insert("model".into(), wire_model.as_str().into());
    Ok(serde_json::Value::Object(body))
}

pub(crate) fn vertex_anthropic_body(
    request: &AnthropicMessagesRequest,
) -> Result<serde_json::Value, &'static str> {
    let mut body = anthropic_parts(request)?;
    body.insert("anthropic_version".into(), "vertex-2023-10-16".into());
    Ok(serde_json::Value::Object(body))
}

fn anthropic_parts(
    request: &AnthropicMessagesRequest,
) -> Result<serde_json::Map<String, serde_json::Value>, &'static str> {
    let mut body = serde_json::Map::new();

    if request.instruction_mode == crate::AnthropicInstructionMode::Epochs
        && !crate::supports_anthropic_system_updates(&request.model)
    {
        return Err("Anthropic instruction epochs require a supported native model");
    }
    if request.effort.updates()
        && !crate::supports_anthropic_effort_updates(
            request.model.provider(),
            request.model.model(),
        )
    {
        return Err("Anthropic effort updates require a supported native model");
    }
    let effort = crate::effort::projection(&request.messages, &request.model, request.effort);
    if let Some(value) = effort.baseline {
        if !value.native() {
            return Err("Anthropic effort does not support this level");
        }
        body.insert(
            "output_config".into(),
            serde_json::json!({"effort":value.as_str()}),
        );
    }
    let cached = request.prompt_cache == AnthropicPromptCache::StablePrefix;
    let (system, mut messages) = super::anthropic_input::project_with_effort(
        &request.messages,
        crate::supports_anthropic_system_updates(&request.model),
        &effort,
    )?;
    let suffix = request
        .uncached_system_suffix
        .as_deref()
        .filter(|text| !text.is_empty());
    if cached {
        let mut blocks = Vec::new();
        if !system.is_empty() {
            blocks.push(serde_json::json!({"type":"text", "text":system.join("\n\n"), "cache_control":native_cache_control()}));
        }
        if let Some(suffix) = suffix {
            blocks.push(serde_json::json!({"type":"text", "text":suffix}));
        }
        if !blocks.is_empty() {
            body.insert("system".into(), serde_json::Value::Array(blocks));
        }
        mark_conversation_breakpoints(&mut messages);
    } else {
        let mut system = system;
        system.extend(suffix);
        if !system.is_empty() {
            body.insert("system".into(), serde_json::json!(system.join("\n\n")));
        }
    }
    body.insert("messages".to_owned(), serde_json::Value::Array(messages));
    body.insert(
        "max_tokens".to_owned(),
        serde_json::Value::from(request.max_tokens.value()),
    );
    body.insert("stream".to_owned(), serde_json::Value::Bool(true));

    if let Some(thinking) = request.thinking {
        body.insert(
            "thinking".to_owned(),
            match thinking {
                crate::ThinkingConfig::Enabled { budget_tokens } => {
                    serde_json::json!({"type": "enabled", "budget_tokens": budget_tokens})
                }
                // Copyright 2023-2026 SpaceXAI. Apache-2.0.
                // Modified by Grapt: closed request enum and fixed summary display.
                crate::ThinkingConfig::Adaptive => {
                    serde_json::json!({"type": "adaptive", "display": "summarized"})
                }
            },
        );
    }

    let mut tools: Vec<serde_json::Value> = request
        .tools
        .iter()
        // Copyright 2023-2026 SpaceXAI, Apache-2.0. Modified by Grapt:
        // project every tool source through Anthropic's schema restriction.
        .map(super::anthropic_tools::project)
        .collect::<Result<_, _>>()?;
    if let Some(search) = &request.web_search {
        // Native search remains in the stable tool prefix, inside the existing
        // last-tool cache boundary rather than consuming another marker.
        tools.insert(0, search.wire());
    }
    if cached {
        if let Some(serde_json::Value::Object(last)) = tools.last_mut() {
            last.insert("cache_control".to_owned(), native_cache_control());
        }
    }
    let tools_present = !tools.is_empty();
    if tools_present {
        body.insert("tools".to_owned(), serde_json::Value::Array(tools));
    }
    // `tool_choice` is emitted ONLY when tools are present.
    //
    // Oracle, same file, at the lowering: "Anthropic rejects tool_choice when
    // tools are absent; `none` is only meaningful with tools present." Emitting
    // it beside an absent `tools` is a provider 400, so the guard sits here —
    // before bytes — rather than being read back out of an error body.
    if tools_present {
        if let Some(choice) = request.tool_choice.as_ref() {
            body.insert("tool_choice".to_owned(), anthropic_tool_choice(choice));
        }
    }
    Ok(body)
}

/// Native Anthropic one-hour ephemeral marker.
fn native_cache_control() -> serde_json::Value {
    serde_json::json!({"type":"ephemeral", "ttl":"1h"})
}

/// Compatible gateways retain their provider-default ephemeral lifetime.
fn cache_control() -> serde_json::Value {
    serde_json::json!({"type": "ephemeral"})
}

// Copyright 2023-2026 SpaceXAI. Licensed under Apache-2.0.
// Modified by Grapt: JSON projection, cacheable-block checks and a shared
// Native four-marker budget including tools.
// apply_cache_breakpoints at 2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8.
fn mark_conversation_breakpoints(messages: &mut [serde_json::Value]) -> usize {
    let Some(tip) = messages.iter().rposition(|message| {
        message
            .get("content")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|blocks| blocks.iter().any(can_carry_cache_control))
    }) else {
        return 0;
    };
    let _tip_marked = mark_message_breakpoint(&mut messages[tip]);
    // Skip the trailing input run, including tool results, mail and System updates.
    // This is the previous input anchor inferred from typed roles, not a
    // claim that a historical HTTP request or provider cache hit was observed.
    let previous = messages[..tip]
        .iter()
        .rposition(|message| message["role"] == "assistant")
        .and_then(|assistant| {
            messages[..assistant].iter().rposition(|message| {
                matches!(message["role"].as_str(), Some("user" | "system"))
                    && message["content"]
                        .as_array()
                        .is_some_and(|blocks| blocks.iter().any(can_carry_cache_control))
            })
        });
    match previous {
        Some(previous) if mark_message_breakpoint(&mut messages[previous]) => 2,
        _ => 1,
    }
}
fn mark_message_breakpoint(message: &mut serde_json::Value) -> bool {
    let block = message
        .get_mut("content")
        .and_then(serde_json::Value::as_array_mut)
        .and_then(|blocks| {
            blocks
                .iter_mut()
                .rev()
                .find(|block| can_carry_cache_control(block))
        });
    if let Some(serde_json::Value::Object(block)) = block {
        block.insert("cache_control".to_owned(), native_cache_control());
        true
    } else {
        false
    }
}

fn can_carry_cache_control(block: &serde_json::Value) -> bool {
    match block.get("type").and_then(serde_json::Value::as_str) {
        Some("tool_use" | "tool_result") => true,
        Some("text") => block
            .get("text")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|text| !text.is_empty()),
        _ => false,
    }
}

/// Anthropic's tool choice is an object, not a string.
///
/// CORRECTION, recorded rather than silently rewritten. This function
/// previously mapped `ToolChoice::None` to `{"type":"auto"}`, justified by a
/// comment asserting "There is no `none` in the Messages API's `tool_choice`".
/// **That assertion was false and I never checked it against the oracle before
/// writing it.** The frozen oracle at `42e4888cc0` carries the schema
/// (the oracle's `anthropic-messages.ts:197`):
///
/// ```text
/// const AnthropicToolChoice = Schema.Union([
///   Schema.Struct({ type: Schema.Literals(["auto", "any", "none"]) }),
///   Schema.Struct({ type: Schema.tag("tool"), name: Schema.String }),
/// ])
/// ```
///
/// `none` exists. Mapping it to `auto` INVERTED the caller's instruction on
/// exactly the case where it matters — tools offered, model told not to call
/// them — so the request that reached the wire permitted the calls the caller
/// had forbidden.
fn anthropic_tool_choice(choice: &ToolChoice) -> serde_json::Value {
    match choice {
        ToolChoice::Auto => serde_json::json!({ "type": "auto" }),
        ToolChoice::None => serde_json::json!({ "type": "none" }),
        ToolChoice::Required => serde_json::json!({ "type": "any" }),
        ToolChoice::Named { tool } => serde_json::json!({ "type": "tool", "name": tool }),
    }
}

// ---------------------------------------------------------------------------
// OpenAI-compatible chat completions
// ---------------------------------------------------------------------------

/// The chat-completions body.
///
/// `max_tokens`, not `max_output_tokens`: this is a different protocol from the
/// Responses API, and using the other spelling would be silently ignored by
/// every server in the family.
///
/// `stream_options.include_usage` is requested because the protocol otherwise
/// reports no usage at all on a streamed response, and a usage-less attempt
/// cannot be reconciled. A server that does not understand the option ignores
/// it, and the trailing usage chunk simply never arrives — which the decoder
/// already treats as absent rather than zero.
pub(crate) fn compatible_body(
    request: &OpenAiCompatibleRequest,
    protocol: crate::CompatibleProtocol,
) -> Result<serde_json::Value, &'static str> {
    let mut body = compatible_parts(request, protocol)?;
    body.insert("model".into(), request.model.model().as_str().into());
    body.insert("stream".into(), true.into());
    body.insert(
        "stream_options".into(),
        serde_json::json!({"include_usage":true}),
    );
    Ok(serde_json::Value::Object(body))
}

pub(crate) fn compatible_parts(
    request: &OpenAiCompatibleRequest,
    protocol: crate::CompatibleProtocol,
) -> Result<serde_json::Map<String, serde_json::Value>, &'static str> {
    let mut body = serde_json::Map::new();
    if let Some(effort) = request.model.variant() {
        body.insert(
            "reasoning_effort".to_owned(),
            serde_json::json!(effort.as_str()),
        );
    }
    let replay = if request.tools.is_empty() {
        crate::CompatibleReasoningReplay::None
    } else {
        request.reasoning_replay
    };
    let mut messages = super::compatible_input::project_with_reasoning(&request.messages, replay)?;
    if request.prompt_cache == CompatiblePromptCache::AnthropicBreakpoints {
        mark_compatible_breakpoints(&mut messages);
    }
    body.insert("messages".to_owned(), serde_json::Value::Array(messages));
    if let Some(bound) = request.max_tokens {
        body.insert(
            match protocol {
                crate::CompatibleProtocol::OpenAi => "max_tokens",
                crate::CompatibleProtocol::Groq | crate::CompatibleProtocol::CompletionTokens => {
                    "max_completion_tokens"
                }
            }
            .to_owned(),
            serde_json::Value::from(bound.value()),
        );
    }
    if protocol == crate::CompatibleProtocol::Groq
        && !request
            .model
            .model()
            .as_str()
            .starts_with("openai/gpt-oss-")
    {
        body.insert("reasoning_format".to_owned(), serde_json::json!("parsed"));
    }

    // The chat-completions tool shape nests the function, unlike Responses'
    // flat one.
    let tools: Vec<serde_json::Value> = request
        .tools
        .iter()
        .map(|tool| {
            let mut function = serde_json::Map::new();
            function.insert("name".to_owned(), serde_json::json!(tool.name));
            function.insert("parameters".to_owned(), schema_of(tool));
            insert_description(&mut function, tool);
            serde_json::json!({"type":"function", "function":function})
        })
        .collect();
    if !tools.is_empty() {
        body.insert("tools".to_owned(), serde_json::Value::Array(tools));
    }
    if let Some(choice) = request.tool_choice.as_ref() {
        body.insert("tool_choice".to_owned(), compatible_tool_choice(choice));
    }
    Ok(body)
}

/// The oracle's four breakpoints for an Anthropic model behind a compatible
/// gateway: the first two system messages and the last two other messages
/// that carry text. A marked message's text becomes one text part carrying
/// the marker, the only placement the gateway forwards to Anthropic; a
/// tool-call-only assistant message has no text to mark and is passed over.
fn mark_compatible_breakpoints(messages: &mut [serde_json::Value]) {
    let has_text = |message: &serde_json::Value| {
        message["content"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
    };
    let system: Vec<usize> = (0..messages.len())
        .filter(|&index| messages[index]["role"] == "system" && has_text(&messages[index]))
        .take(2)
        .collect();
    let tail: Vec<usize> = (0..messages.len())
        .rev()
        .filter(|&index| messages[index]["role"] != "system" && has_text(&messages[index]))
        .take(2)
        .collect();
    for index in system.into_iter().chain(tail) {
        let text = messages[index]["content"].take();
        messages[index]["content"] =
            serde_json::json!([{"type": "text", "text": text, "cache_control": cache_control()}]);
    }
}

fn compatible_tool_choice(choice: &ToolChoice) -> serde_json::Value {
    match choice {
        ToolChoice::Auto => serde_json::Value::String("auto".to_owned()),
        ToolChoice::None => serde_json::Value::String("none".to_owned()),
        ToolChoice::Required => serde_json::Value::String("required".to_owned()),
        ToolChoice::Named { tool } => {
            serde_json::json!({ "type": "function", "function": { "name": tool } })
        }
    }
}

#[cfg(test)]
mod tests;
