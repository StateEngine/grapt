//! The emitted request-body key sets, asserted directly.
//!
//! Law: `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md`
//! §"Provider request boundary" and its mutation table row "Add
//! `max_output_tokens` to `ChatGptCodexRequest` -> Rust compile failure."
//!
//! # Why a key-set assertion exists beside a compile-fail probe
//!
//! Two different failures:
//!
//! - `tests/compile_fail.rs` probe 04 proves a CALLER cannot set
//!   `max_output_tokens` on the Codex route, because the field does not exist.
//! - This file proves the SERIALIZER does not insert it by string. A type
//!   cannot stop `body.insert("max_output_tokens", ...)`, and the oracle's
//!   defect was precisely in the serializer
//!   (the oracle's `openai-responses.ts:122` builds the key and
//!   deletes it again).
//!
//! Either layer alone is weaker than both. The oracle had only the second kind,
//! as an absence assertion, and the regression recurred anyway.
//!
//! # These assert the WHOLE key set, not the absence of one key
//!
//! An absence assertion (`!body.contains_key("max_output_tokens")`) passes
//! against a builder that emits nothing at all, and against one that emits
//! fifty keys as long as that one is missing. Asserting the exact key set
//! catches a field silently gained as well as one silently lost.

use crate::request::{
    AnthropicMessagesRequest, ChatGptCodexRequest, CodexInputItem, CodexToolText, InputMessage,
    MaxOutputTokens, MessageRole, OpenAiCompatibleRequest, OpenAiResponsesRequest, PromptCacheKey,
    ProviderRequest, ThinkingConfig, ToolChoice, ToolDefinition,
};
use grapt_policy::ToolName;
use grapt_types::{
    BaseModelId, ModelCatalog, ModelCatalogRecord, ModelCatalogVersion, ModelFamilyId, ModelId,
    ModelRef, ProviderId, SessionId, ToolCallId,
};

fn model(vendor: &str, name: &str) -> ModelRef {
    model_variant(vendor, name, None)
}

fn model_variant(
    vendor: &str,
    name: &str,
    variant: Option<grapt_types::ModelVariantId>,
) -> ModelRef {
    let (provider, model, base, family, version) = match (
        ProviderId::new(vendor),
        ModelId::new(name),
        BaseModelId::new(name),
        ModelFamilyId::new(vendor),
        ModelCatalogVersion::new("test-catalog"),
    ) {
        (Some(p), Some(m), Some(b), Some(f), Some(v)) => (p, m, b, f, v),
        _ => panic!("test model coordinates must be non-empty"),
    };
    let record = ModelCatalogRecord::new(
        provider.clone(),
        model.clone(),
        base,
        family,
        variant.clone(),
    );
    let catalog = match ModelCatalog::load(version, vec![record]) {
        Some(catalog) => catalog,
        None => panic!("test catalog"),
    };
    match catalog.entry(&provider, &model, variant.as_ref()) {
        Some(entry) => ModelRef::from_catalog(entry),
        None => panic!("catalog entry"),
    }
}

/// The reasoning the account catalog asks for (Codex 0.155.1
/// `build_reasoning`): every turn's reasoning for a Responses Lite model, a
/// summary for a model that supports one, beside any selected effort. A
/// model with neither is unchanged.
#[test]
fn the_catalog_reasoning_options_reach_the_codex_body() -> Result<(), Box<dyn std::error::Error>> {
    let codex = |variant: Option<&str>, all_turns: bool, summary: bool| {
        ProviderRequest::ChatGptCodex(ChatGptCodexRequest {
            model: model_variant(
                "openai",
                "selected-model",
                variant.and_then(grapt_types::ModelVariantId::new),
            ),
            input: vec![user("Hello").into()],
            prompt_cache_key: None,
            tools: Vec::new(),
            web_search: None,
            tool_choice: None,
            reasoning: crate::CodexReasoning {
                all_turns,
                summary,
                ..Default::default()
            },
            turn: Default::default(),
        })
    };
    assert_eq!(
        body(&codex(None, true, false))["reasoning"],
        serde_json::json!({"context":"all_turns"})
    );
    assert_eq!(
        body(&codex(None, false, true))["reasoning"],
        serde_json::json!({"summary":"auto"})
    );
    assert_eq!(
        body(&codex(Some("high"), true, true))["reasoning"],
        serde_json::json!({"effort":"high","context":"all_turns","summary":"auto"})
    );
    assert!(body(&codex(None, false, false)).get("reasoning").is_none());
    assert_eq!(
        body(&codex(Some("high"), false, false))["reasoning"],
        serde_json::json!({"effort":"high"})
    );
    // Only the body changes: no Codex-internal Responses Lite header, so the
    // request keeps parallel tool calls (that header requires them off).
    assert!(body(&codex(None, true, true))
        .get("parallel_tool_calls")
        .is_none());
    Ok(())
}

#[test]
fn selected_reasoning_effort_reaches_codex_request_body() -> Result<(), Box<dyn std::error::Error>>
{
    let request = ProviderRequest::ChatGptCodex(ChatGptCodexRequest {
        model: model_variant(
            "openai",
            "selected-model",
            Some(grapt_types::ModelVariantId::new("high").ok_or("effort")?),
        ),
        input: vec![user("Hello").into()],
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
        reasoning: Default::default(),
        turn: Default::default(),
    });
    assert_eq!(
        keys(&request),
        vec!["include", "input", "model", "reasoning", "store", "stream"]
    );
    assert_eq!(body(&request)["model"], "selected-model");
    assert_eq!(
        body(&request)["reasoning"],
        serde_json::json!({"effort":"high"})
    );
    Ok(())
}

fn bound(value: u32) -> MaxOutputTokens {
    match MaxOutputTokens::new(value) {
        Some(bound) => bound,
        None => panic!("a non-zero bound"),
    }
}

fn user(text: &str) -> InputMessage {
    InputMessage {
        role: MessageRole::User,
        content: text.to_owned(),
    }
}

fn system(text: &str) -> InputMessage {
    InputMessage {
        role: MessageRole::System,
        content: text.to_owned(),
    }
}

fn tool() -> ToolDefinition {
    ToolDefinition {
        input_format: crate::ToolInputFormat::Json,
        name: "read_file".to_owned(),
        description: Some("Read a file".to_owned()),
        input_schema: r#"{"type":"object"}"#.to_owned(),
    }
}

/// Serialize through the same dispatch the adapter uses.
///
/// In-crate rather than an integration test, deliberately: exposing a public
/// `request_body(...)` would hand a caller the bytes, and the contract reserves
/// that to the adapter ("The selected adapter alone may serialize its request
/// into HTTP bytes"). A test-only public function is still a public function.
fn body(request: &ProviderRequest) -> serde_json::Value {
    let result = match request {
        ProviderRequest::OpenAiResponses(request) => super::openai_responses_body(request),
        ProviderRequest::ChatGptCodex(request) => super::codex_body(request),
        ProviderRequest::AnthropicMessages(request) | ProviderRequest::VertexAnthropic(request) => {
            super::anthropic_body(request)
        }
        ProviderRequest::OpenAiCompatible(request) => {
            super::compatible_body(request, crate::CompatibleProtocol::OpenAi)
        }
        ProviderRequest::BedrockConverse(request) => crate::routes::bedrock::body(request),
        ProviderRequest::GoogleGemini(request) | ProviderRequest::VertexGemini(request) => {
            crate::gemini::body(request)
        }
        ProviderRequest::CohereChat(request) => crate::cohere::body(request),
        ProviderRequest::WatsonxChat(request) => {
            grapt_types::WatsonxProjectId::new("fixture-project")
                .ok_or("fixture project")
                .and_then(|project| crate::watsonx::body(request, &project))
        }
    };
    result.unwrap_or_else(|error| panic!("fixture body: {error}"))
}

/// The emitted top-level key set, sorted.
fn keys(request: &ProviderRequest) -> Vec<String> {
    let body = body(request);
    let object = match body.as_object() {
        Some(object) => object,
        None => panic!("a request body must be a JSON object"),
    };
    let mut keys: Vec<String> = object.keys().cloned().collect();
    keys.sort();
    keys
}

// ---------------------------------------------------------------------------
// The regression the contract names
// ---------------------------------------------------------------------------

#[test]
fn the_codex_body_has_no_output_token_key_under_any_name() {
    let request = ProviderRequest::ChatGptCodex(ChatGptCodexRequest {
        model: model("openai", "gpt-5-codex"),
        input: vec![user("Hello").into()],
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
        reasoning: Default::default(),
        turn: Default::default(),
    });
    assert_eq!(
        keys(&request),
        vec!["include", "input", "model", "store", "stream"]
    );
    assert_eq!(body(&request).get("store"), Some(&serde_json::json!(false)));

    // Named explicitly as well as by the set, because this is the one key the
    // contract calls out by name and a reader should be able to grep for it.
    let body = body(&request);
    for name in ["max_output_tokens", "max_tokens", "max_completion_tokens"] {
        assert!(
            body.get(name).is_none(),
            "the Codex route must not carry {name}"
        );
    }
}

#[test]
fn openai_responses_families_emit_the_typed_prompt_cache_key_only_when_present(
) -> Result<(), Box<dyn std::error::Error>> {
    let session: SessionId = "ses_cache_body".parse()?;
    let key = PromptCacheKey::for_session(&session)?;
    let codex = ProviderRequest::ChatGptCodex(ChatGptCodexRequest {
        model: model("openai", "gpt-5-codex"),
        input: vec![user("Hello").into()],
        prompt_cache_key: Some(key.clone()),
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
        reasoning: Default::default(),
        turn: Default::default(),
    });
    let responses = ProviderRequest::OpenAiResponses(OpenAiResponsesRequest {
        model: model("openai", "gpt-5"),
        input: vec![user("Hello").into()],
        prompt_cache_key: Some(key.clone()),
        max_output_tokens: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
    });

    assert_eq!(
        body(&codex)["prompt_cache_key"],
        key.as_str(),
        "the ChatGPT Codex body dropped its typed provider cache coordinate"
    );
    assert_eq!(
        body(&responses)["prompt_cache_key"],
        key.as_str(),
        "the OpenAI Responses body dropped its typed provider cache coordinate"
    );
    assert_eq!(
        keys(&codex),
        vec![
            "include",
            "input",
            "model",
            "prompt_cache_key",
            "store",
            "stream"
        ]
    );
    Ok(())
}

#[test]
fn codex_projects_typed_tool_call_and_output_only_at_the_route_adapter() {
    let call = ToolCallId::new("call_durable").unwrap_or_else(|| panic!("test call id"));
    let tool_name = ToolName::new("read").unwrap_or_else(|| panic!("test tool name"));
    let arguments = CodexToolText::try_new(r#"{"path":"tool.txt"}"#.to_owned())
        .unwrap_or_else(|error| panic!("test arguments: {error}"));
    let output = CodexToolText::try_new("exact durable output".to_owned())
        .unwrap_or_else(|error| panic!("test output: {error}"));
    let request = ProviderRequest::ChatGptCodex(ChatGptCodexRequest {
        model: model("openai", "gpt-5-codex"),
        input: vec![
            user("read tool.txt").into(),
            CodexInputItem::ToolCall {
                call: call.clone(),
                tool: tool_name,
                arguments,
            },
            CodexInputItem::ToolOutput {
                call,
                output,
                is_error: false,
            },
        ],
        prompt_cache_key: None,
        tools: vec![tool()],
        web_search: None,
        tool_choice: Some(ToolChoice::Auto),
        reasoning: Default::default(),
        turn: Default::default(),
    });
    assert_eq!(
        body(&request)["input"],
        serde_json::json!([
            {"role": "user", "content": "read tool.txt"},
            {
                "type": "function_call",
                "call_id": "call_durable",
                "name": "read",
                "arguments": "{\"path\":\"tool.txt\"}"
            },
            {
                "type": "function_call_output",
                "call_id": "call_durable",
                "output": "exact durable output"
            }
        ])
    );
}

#[test]
fn the_openai_responses_body_does_carry_its_bound() {
    // The field is not banned globally — it is banned on the route that rejects
    // it. A blanket removal would be a different, wrong fix, and this is the
    // assertion that would catch it.
    let request = ProviderRequest::OpenAiResponses(OpenAiResponsesRequest {
        model: model("openai", "gpt-5"),
        input: vec![user("Hello").into()],
        max_output_tokens: Some(bound(4096)),
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
    });
    assert_eq!(
        keys(&request),
        vec!["input", "max_output_tokens", "model", "stream"]
    );
    assert_eq!(
        body(&request)
            .get("max_output_tokens")
            .and_then(|v| v.as_u64()),
        Some(4096)
    );
}

#[test]
fn an_absent_optional_bound_emits_no_key_rather_than_a_null() {
    // A `null` is not the same request as an absent key: several servers reject
    // an explicit null where they accept omission.
    let request = ProviderRequest::OpenAiResponses(OpenAiResponsesRequest {
        model: model("openai", "gpt-5"),
        input: vec![user("Hello").into()],
        max_output_tokens: None,
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
    });
    assert_eq!(keys(&request), vec!["input", "model", "stream"]);
}

// ---------------------------------------------------------------------------
// Each family's own shape
// ---------------------------------------------------------------------------

#[test]
fn the_anthropic_body_requires_its_bound_and_lifts_the_system_prompt() {
    let request = ProviderRequest::AnthropicMessages(AnthropicMessagesRequest {
        web_search: None,
        uncached_system_suffix: None,
        effort: crate::EffortPolicy::default(),
        model: model("anthropic", "claude-opus-5"),
        messages: vec![system("be brief").into(), user("Hello").into()],
        max_tokens: bound(8192),
        instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
        thinking: None,
        prompt_cache: crate::AnthropicPromptCache::Off,
        tools: Vec::new(),
        tool_choice: None,
    });
    assert_eq!(
        keys(&request),
        vec!["max_tokens", "messages", "model", "stream", "system"]
    );

    let body = body(&request);
    // The system message is LIFTED OUT of the turn list. Sending it as a
    // message is accepted by the API and silently changes its meaning, so the
    // turn list must contain only the real turn.
    assert_eq!(
        body.get("system").and_then(|v| v.as_str()),
        Some("be brief")
    );
    let messages = match body.get("messages").and_then(|v| v.as_array()) {
        Some(messages) => messages,
        None => panic!("messages must be an array"),
    };
    assert_eq!(messages.len(), 1, "the system turn must not remain a turn");
    assert_eq!(
        messages[0].get("role").and_then(|v| v.as_str()),
        Some("user")
    );
    // Anthropic's bound is required, so it is always present — never omitted
    // the way the OpenAI routes' optional one is.
    assert_eq!(body.get("max_tokens").and_then(|v| v.as_u64()), Some(8192));
}

#[test]
fn multiple_system_messages_are_joined_rather_than_dropped() {
    // Keeping only the last would silently change the request the caller made.
    let request = ProviderRequest::AnthropicMessages(AnthropicMessagesRequest {
        web_search: None,
        uncached_system_suffix: None,
        effort: crate::EffortPolicy::default(),
        model: model("anthropic", "claude-opus-5"),
        messages: vec![
            system("first").into(),
            system("second").into(),
            user("Hi").into(),
        ],
        max_tokens: bound(16),
        instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
        thinking: None,
        prompt_cache: crate::AnthropicPromptCache::Off,
        tools: Vec::new(),
        tool_choice: None,
    });
    assert_eq!(
        body(&request).get("system").and_then(|v| v.as_str()),
        Some("first\n\nsecond")
    );
}

#[test]
fn anthropic_thinking_is_emitted_only_when_asked_for() {
    let without = ProviderRequest::AnthropicMessages(AnthropicMessagesRequest {
        web_search: None,
        uncached_system_suffix: None,
        effort: crate::EffortPolicy::default(),
        model: model("anthropic", "claude-opus-5"),
        messages: vec![user("Hi").into()],
        max_tokens: bound(16),
        instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
        thinking: None,
        prompt_cache: crate::AnthropicPromptCache::Off,
        tools: Vec::new(),
        tool_choice: None,
    });
    assert!(!keys(&without).contains(&"thinking".to_owned()));

    let with = ProviderRequest::AnthropicMessages(AnthropicMessagesRequest {
        web_search: None,
        uncached_system_suffix: None,
        effort: crate::EffortPolicy::default(),
        model: model("anthropic", "claude-opus-5"),
        messages: vec![user("Hi").into()],
        max_tokens: bound(16),
        instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
        thinking: Some(ThinkingConfig::Enabled {
            budget_tokens: 2048,
        }),
        prompt_cache: crate::AnthropicPromptCache::Off,
        tools: Vec::new(),
        tool_choice: None,
    });
    let body = body(&with);
    let thinking = match body.get("thinking") {
        Some(thinking) => thinking,
        None => panic!("thinking must be emitted when configured"),
    };
    assert_eq!(
        thinking.get("type").and_then(|v| v.as_str()),
        Some("enabled")
    );
    assert_eq!(
        thinking.get("budget_tokens").and_then(|v| v.as_u64()),
        Some(2048)
    );
}

#[test]
fn an_anthropic_model_behind_a_compatible_gateway_gets_the_oracles_four_breakpoints() {
    let call = grapt_types::ToolCallId::new("call_cache").unwrap_or_else(|| panic!("call"));
    let tool = crate::ToolName::new("read").unwrap_or_else(|| panic!("tool"));
    let bounded = |text: &str| {
        crate::CodexToolText::try_new(text.into()).unwrap_or_else(|error| panic!("text: {error}"))
    };
    let request = |prompt_cache| OpenAiCompatibleRequest {
        reasoning_replay: Default::default(),
        prompt_cache,
        model: model("openrouter", "anthropic/claude-sonnet-5.5"),
        messages: vec![
            system("base").into(),
            system("environment").into(),
            system("date").into(),
            user("first").into(),
            user("second").into(),
            crate::CodexInputItem::ToolCall {
                call: call.clone(),
                tool: tool.clone(),
                arguments: bounded("{}"),
            },
            crate::CodexInputItem::ToolOutput {
                is_error: false,
                call: call.clone(),
                output: bounded("file text"),
            },
        ],
        max_tokens: None,
        tools: vec![],
        tool_choice: None,
        routing_session: None,
    };
    let marked = |text: &str| serde_json::json!([{"type": "text", "text": text, "cache_control": {"type": "ephemeral"}}]);
    let emitted = super::compatible_body(
        &request(crate::CompatiblePromptCache::AnthropicBreakpoints),
        crate::CompatibleProtocol::OpenAi,
    )
    .unwrap_or_else(|error| panic!("body: {error}"));
    let messages = &emitted["messages"];
    // The first two system messages and the last two other messages with
    // text; the tool-call-only assistant message has nothing to mark.
    assert_eq!(messages[0]["content"], marked("base"));
    assert_eq!(messages[1]["content"], marked("environment"));
    assert_eq!(messages[2]["content"], "date");
    assert_eq!(messages[3]["content"], "first");
    assert_eq!(messages[4]["content"], marked("second"));
    assert!(messages[5].get("content").is_none());
    assert_eq!(messages[6]["content"], marked("file text"));
    assert_eq!(emitted.to_string().matches("cache_control").count(), 4);
    let plain = super::compatible_body(
        &request(crate::CompatiblePromptCache::Off),
        crate::CompatibleProtocol::OpenAi,
    )
    .unwrap_or_else(|error| panic!("body: {error}"));
    assert!(!plain.to_string().contains("cache_control"));
}

#[test]
fn compatible_history_preserves_call_identity_arguments_and_output() {
    let call = grapt_types::ToolCallId::new("call_history").unwrap_or_else(|| panic!("call"));
    let tool = crate::ToolName::new("journey_echo").unwrap_or_else(|| panic!("tool"));
    let bounded = |text: &str| {
        crate::CodexToolText::try_new(text.into()).unwrap_or_else(|error| panic!("text: {error}"))
    };
    let request = OpenAiCompatibleRequest {
        reasoning_replay: Default::default(),
        prompt_cache: crate::CompatiblePromptCache::Off,
        model: model("xai", "grok-4"),
        messages: vec![
            user("hello").into(),
            crate::CodexInputItem::ToolCall {
                call: call.clone(),
                tool,
                arguments: bounded(r#"{"text":"hello"}"#),
            },
            crate::CodexInputItem::ToolOutput {
                is_error: false,
                call,
                output: bounded("actual-result"),
            },
        ],
        max_tokens: None,
        tools: vec![],
        tool_choice: None,
        routing_session: None,
    };
    let emitted = super::compatible_body(&request, crate::CompatibleProtocol::OpenAi)
        .unwrap_or_else(|error| panic!("body: {error}"));
    assert_eq!(
        emitted["messages"],
        serde_json::json!([
            {"role":"user","content":"hello"},
            {"role":"assistant","tool_calls":[{"id":"call_history","type":"function","function":{"name":"journey_echo","arguments":"{\"text\":\"hello\"}"}}]},
            {"role":"tool","tool_call_id":"call_history","content":"actual-result"}
        ])
    );
}

#[test]
fn the_compatible_body_uses_max_tokens_and_asks_for_usage() {
    let request = ProviderRequest::OpenAiCompatible(OpenAiCompatibleRequest {
        reasoning_replay: Default::default(),
        prompt_cache: crate::CompatiblePromptCache::Off,
        model: model("xai", "grok-4"),
        messages: vec![user("Hello").into()],
        max_tokens: Some(bound(1024)),
        tools: Vec::new(),
        tool_choice: None,
        routing_session: None,
    });
    assert_eq!(
        keys(&request),
        vec![
            "max_tokens",
            "messages",
            "model",
            "stream",
            "stream_options"
        ]
    );

    let body = body(&request);
    // `max_tokens`, NOT `max_output_tokens`: this is a different protocol from
    // the Responses API, and the other spelling is silently ignored by every
    // server in this family — the request would run unbounded.
    assert_eq!(body.get("max_tokens").and_then(|v| v.as_u64()), Some(1024));
    assert!(body.get("max_output_tokens").is_none());

    // Without this the protocol reports NO usage at all on a streamed
    // response, and an attempt with no usage cannot be reconciled.
    assert_eq!(
        body.get("stream_options")
            .and_then(|options| options.get("include_usage"))
            .and_then(|v| v.as_bool()),
        Some(true)
    );
}

// ---------------------------------------------------------------------------
// Tool shapes differ per family, and the difference is load-bearing
// ---------------------------------------------------------------------------

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn each_family_emits_its_own_tool_shape() {
    // A shared tool shape would be accepted as JSON by all three and understood
    // by at most one. These are the three real shapes.

    // Responses: flat, `parameters`.
    let responses = ProviderRequest::OpenAiResponses(OpenAiResponsesRequest {
        model: model("openai", "gpt-5"),
        input: vec![user("Hi").into()],
        max_output_tokens: None,
        prompt_cache_key: None,
        tools: vec![tool()],
        web_search: None,
        tool_choice: Some(ToolChoice::Auto),
    });
    let emitted = body(&responses);
    let tools = match emitted.get("tools").and_then(|v| v.as_array()) {
        Some(tools) => tools,
        None => panic!("tools must be an array"),
    };
    assert_eq!(
        tools[0].get("name").and_then(|v| v.as_str()),
        Some("read_file")
    );
    assert_eq!(tools[0]["description"], "Read a file");
    assert!(tools[0].get("parameters").is_some());
    assert!(
        tools[0].get("function").is_none(),
        "the Responses shape is flat, not nested"
    );

    // Anthropic: `input_schema`, no `type`.
    let anthropic = ProviderRequest::AnthropicMessages(AnthropicMessagesRequest {
        web_search: None,
        uncached_system_suffix: None,
        effort: crate::EffortPolicy::default(),
        model: model("anthropic", "claude-opus-5"),
        messages: vec![user("Hi").into()],
        max_tokens: bound(16),
        instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
        thinking: None,
        prompt_cache: crate::AnthropicPromptCache::Off,
        tools: vec![tool()],
        tool_choice: Some(ToolChoice::Required),
    });
    let emitted = body(&anthropic);
    let tools = match emitted.get("tools").and_then(|v| v.as_array()) {
        Some(tools) => tools,
        None => panic!("tools must be an array"),
    };
    assert!(
        tools[0].get("input_schema").is_some(),
        "Anthropic names the schema input_schema, not parameters"
    );
    assert!(tools[0].get("parameters").is_none());
    assert_eq!(tools[0]["description"], "Read a file");
    // Anthropic's tool_choice is an OBJECT, and `required` is spelled `any`.
    assert_eq!(
        emitted
            .get("tool_choice")
            .and_then(|choice| choice.get("type"))
            .and_then(|v| v.as_str()),
        Some("any")
    );

    // Chat completions: nested under `function`.
    let compat = ProviderRequest::OpenAiCompatible(OpenAiCompatibleRequest {
        reasoning_replay: Default::default(),
        prompt_cache: crate::CompatiblePromptCache::Off,
        model: model("xai", "grok-4"),
        messages: vec![user("Hi").into()],
        max_tokens: None,
        tools: vec![tool()],
        tool_choice: Some(ToolChoice::Named {
            tool: "read_file".to_owned(),
        }),
        routing_session: None,
    });
    let emitted = body(&compat);
    let tools = match emitted.get("tools").and_then(|v| v.as_array()) {
        Some(tools) => tools,
        None => panic!("tools must be an array"),
    };
    assert_eq!(
        tools[0]
            .get("function")
            .and_then(|function| function.get("name"))
            .and_then(|v| v.as_str()),
        Some("read_file"),
        "chat-completions nests the function"
    );
    assert_eq!(tools[0]["function"]["description"], "Read a file");
    assert_eq!(
        emitted
            .get("tool_choice")
            .and_then(|choice| choice.get("function"))
            .and_then(|function| function.get("name"))
            .and_then(|v| v.as_str()),
        Some("read_file")
    );

    let codex = ProviderRequest::ChatGptCodex(ChatGptCodexRequest {
        model: model("openai", "gpt-5-codex"),
        input: vec![user("Hi").into()],
        prompt_cache_key: None,
        tools: vec![tool()],
        web_search: None,
        tool_choice: Some(ToolChoice::Auto),
        reasoning: Default::default(),
        turn: Default::default(),
    });
    assert_eq!(body(&codex)["tools"][0]["description"], "Read a file");

    let mut absent = tool();
    absent.description = None;
    let without_description = ProviderRequest::OpenAiResponses(OpenAiResponsesRequest {
        model: model("openai", "gpt-5"),
        input: vec![user("Hi").into()],
        max_output_tokens: None,
        prompt_cache_key: None,
        tools: vec![absent],
        web_search: None,
        tool_choice: None,
    });
    assert!(body(&without_description)["tools"][0]
        .get("description")
        .is_none());
}

#[test]
fn anthropic_preserves_an_explicit_tool_choice_none() {
    // POSITIVE. `none` exists in the Messages API schema
    // (oracle `anthropic-messages.ts:197`, literals ["auto","any","none"]).
    // This previously emitted `{"type":"auto"}`, which INVERTED the caller's
    // instruction on exactly the case where it matters: tools offered, model
    // told not to call them, request sent permitting the calls the caller had
    // forbidden.
    let request = ProviderRequest::AnthropicMessages(AnthropicMessagesRequest {
        web_search: None,
        uncached_system_suffix: None,
        effort: crate::EffortPolicy::default(),
        model: model("anthropic", "claude-opus-5"),
        messages: vec![user("Hi").into()],
        max_tokens: bound(16),
        instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
        thinking: None,
        prompt_cache: crate::AnthropicPromptCache::Off,
        tools: vec![tool()],
        tool_choice: Some(ToolChoice::None),
    });
    let emitted = body(&request);
    assert_eq!(
        emitted
            .get("tool_choice")
            .and_then(|choice| choice.get("type"))
            .and_then(|v| v.as_str()),
        Some("none"),
        "an explicit None must travel as none, never as auto"
    );
}

#[test]
fn an_empty_tool_list_emits_no_tools_key() {
    // An empty `tools: []` is not the same request as no tools: some servers
    // treat the empty array as "tools are configured but none are available"
    // and refuse a tool_choice alongside it.
    let request = ProviderRequest::ChatGptCodex(ChatGptCodexRequest {
        model: model("openai", "gpt-5-codex"),
        input: vec![user("Hi").into()],
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
        reasoning: Default::default(),
        turn: Default::default(),
    });
    assert!(!keys(&request).contains(&"tools".to_owned()));
}

#[test]
fn every_family_streams() {
    // A body that forgot `stream: true` returns one non-SSE JSON document, and
    // the SSE decoder would report it as a stream that ended without a terminal
    // — a retryable transport failure for a request that actually succeeded.
    for request in [
        ProviderRequest::ChatGptCodex(ChatGptCodexRequest {
            model: model("openai", "gpt-5-codex"),
            input: vec![user("Hi").into()],
            prompt_cache_key: None,
            tools: Vec::new(),
            web_search: None,
            tool_choice: None,
            reasoning: Default::default(),
            turn: Default::default(),
        }),
        ProviderRequest::OpenAiResponses(OpenAiResponsesRequest {
            model: model("openai", "gpt-5"),
            input: vec![user("Hi").into()],
            max_output_tokens: None,
            prompt_cache_key: None,
            tools: Vec::new(),
            web_search: None,
            tool_choice: None,
        }),
        ProviderRequest::AnthropicMessages(AnthropicMessagesRequest {
            web_search: None,
            uncached_system_suffix: None,
            effort: crate::EffortPolicy::default(),
            model: model("anthropic", "claude-opus-5"),
            messages: vec![user("Hi").into()],
            max_tokens: bound(16),
            instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
            thinking: None,
            prompt_cache: crate::AnthropicPromptCache::Off,
            tools: Vec::new(),
            tool_choice: None,
        }),
        ProviderRequest::OpenAiCompatible(OpenAiCompatibleRequest {
            reasoning_replay: Default::default(),
            prompt_cache: crate::CompatiblePromptCache::Off,
            model: model("xai", "grok-4"),
            messages: vec![user("Hi").into()],
            max_tokens: None,
            tools: Vec::new(),
            tool_choice: None,
            routing_session: None,
        }),
    ] {
        let family = request.family();
        assert_eq!(
            body(&request).get("stream").and_then(|v| v.as_bool()),
            Some(true),
            "{family:?} must request a stream"
        );
    }
}

#[test]
fn the_model_on_the_wire_is_the_catalogs_model_id() {
    // Not the base model, not the family, not a marketing name the caller
    // asserted: the catalog row's own model id, which is the coordinate the
    // provider resolves.
    let request = ProviderRequest::AnthropicMessages(AnthropicMessagesRequest {
        web_search: None,
        uncached_system_suffix: None,
        effort: crate::EffortPolicy::default(),
        model: model("anthropic", "claude-opus-5"),
        messages: vec![user("Hi").into()],
        max_tokens: bound(16),
        instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
        thinking: None,
        prompt_cache: crate::AnthropicPromptCache::Off,
        tools: Vec::new(),
        tool_choice: None,
    });
    assert_eq!(
        body(&request).get("model").and_then(|v| v.as_str()),
        Some("claude-opus-5")
    );
}

mod cache;
mod native_mail;

mod anthropic_search;
mod anthropic_tools;
#[path = "tests/images.rs"]
mod images;
mod web_search;

mod effort;

mod invalid_arguments;
mod request_effort;
mod text_tools;

mod compatible_reasoning;

mod replay_review;
