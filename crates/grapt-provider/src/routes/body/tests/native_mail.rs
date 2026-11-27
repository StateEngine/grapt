//! Offline N02 comparison through the production serializers. These fixtures
//! are synthetic; shape and invented usage counters never establish API hits.

use super::{body, bound, model, system, tool, user};
use crate::request::{
    AnthropicMessagesRequest, AnthropicPromptCache, ChatGptCodexRequest, CodexInputItem,
    CodexToolText, CompatiblePromptCache, OpenAiCompatibleRequest, OpenAiResponsesRequest,
    PromptCacheKey, ProtocolFamily, ProviderRequest, ToolDefinition,
};
use crate::usage::{
    normalize_anthropic_usage, normalize_compatible_usage, normalize_responses_usage, TokenUsage,
    UsageNormalizationError, UsageRoute,
};
use grapt_types::usage::{UsageDerivation, UsageProvenance};
use grapt_types::ToolCallId;
use serde_json::{json, Value};

const FAMILIES: [ProtocolFamily; 4] = [
    ProtocolFamily::OpenAiResponses,
    ProtocolFamily::ChatGptCodex,
    ProtocolFamily::AnthropicMessages,
    ProtocolFamily::OpenAiCompatible,
];

#[expect(clippy::too_many_lines, reason = "legacy")]
fn request(family: ProtocolFamily, input: Vec<CodexInputItem>, cached: bool) -> ProviderRequest {
    let tools = vec![
        tool(),
        ToolDefinition {
            input_format: crate::ToolInputFormat::Json,
            name: "fixture_inbox_read".into(),
            description: Some("Synthetic authenticated-inbox result fixture; no transport".into()),
            input_schema: r#"{"type":"object","properties":{"id":{"type":"string"}}}"#.into(),
        },
    ];
    let key = Some(
        PromptCacheKey::try_new("native-mail-session".into())
            .unwrap_or_else(|error| panic!("fixture cache key: {error}")),
    );
    match family {
        ProtocolFamily::BedrockConverse => panic!("native Bedrock is covered by its wire fixture"),
        ProtocolFamily::GoogleGemini => ProviderRequest::GoogleGemini(crate::GoogleGeminiRequest {
            model: model("google", "fixture-model"),
            messages: input,
            tools,
            max_tokens: None,
            tool_choice: None,
        }),
        ProtocolFamily::CohereChat => ProviderRequest::CohereChat(crate::CohereChatRequest {
            model: model("cohere", "fixture-model"),
            messages: input,
            tools,
            max_tokens: None,
            tool_choice: None,
        }),
        ProtocolFamily::VertexGemini => ProviderRequest::VertexGemini(crate::GoogleGeminiRequest {
            model: model("google", "fixture-model"),
            messages: input,
            tools,
            max_tokens: None,
            tool_choice: None,
        }),
        ProtocolFamily::WatsonxChat => ProviderRequest::WatsonxChat(OpenAiCompatibleRequest {
            reasoning_replay: Default::default(),
            model: model("watsonx", "fixture-model"),
            messages: input,
            tools,
            max_tokens: None,
            tool_choice: None,
            routing_session: None,
            prompt_cache: CompatiblePromptCache::Off,
        }),
        ProtocolFamily::OpenAiResponses => {
            ProviderRequest::OpenAiResponses(OpenAiResponsesRequest {
                model: model("openai", "fixture-model"),
                input,
                tools,
                prompt_cache_key: key,
                max_output_tokens: None,
                web_search: None,
                tool_choice: None,
            })
        }
        ProtocolFamily::ChatGptCodex => ProviderRequest::ChatGptCodex(ChatGptCodexRequest {
            model: model("openai", "fixture-model"),
            input,
            tools,
            prompt_cache_key: key,
            web_search: None,
            tool_choice: None,
            reasoning: Default::default(),
            turn: Default::default(),
        }),
        ProtocolFamily::AnthropicMessages => {
            ProviderRequest::AnthropicMessages(AnthropicMessagesRequest {
                web_search: None,
                uncached_system_suffix: None,
                effort: crate::EffortPolicy::default(),
                model: model("anthropic", "fixture-model"),
                messages: input,
                tools,
                max_tokens: bound(8192),
                instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
                thinking: None,
                tool_choice: None,
                prompt_cache: if cached {
                    AnthropicPromptCache::StablePrefix
                } else {
                    AnthropicPromptCache::Off
                },
            })
        }
        ProtocolFamily::VertexAnthropic => {
            ProviderRequest::VertexAnthropic(AnthropicMessagesRequest {
                web_search: None,
                uncached_system_suffix: None,
                effort: crate::EffortPolicy::default(),
                model: model("anthropic", "fixture-model"),
                messages: input,
                tools,
                max_tokens: bound(8192),
                instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
                thinking: None,
                tool_choice: None,
                prompt_cache: if cached {
                    AnthropicPromptCache::StablePrefix
                } else {
                    AnthropicPromptCache::Off
                },
            })
        }
        ProtocolFamily::OpenAiCompatible => {
            ProviderRequest::OpenAiCompatible(OpenAiCompatibleRequest {
                reasoning_replay: Default::default(),
                model: model("fixture-gateway", "fixture-model"),
                messages: input,
                tools,
                max_tokens: None,
                tool_choice: None,
                routing_session: None,
                prompt_cache: if cached {
                    CompatiblePromptCache::AnthropicBreakpoints
                } else {
                    CompatiblePromptCache::Off
                },
            })
        }
    }
}

fn tool_exchange(id: &str, name: &str, arguments: &str, output: &str) -> [CodexInputItem; 3] {
    let call = ToolCallId::new(id).unwrap_or_else(|| panic!("fixture call"));
    let bounded = |text: &str| {
        CodexToolText::try_new(text.into())
            .unwrap_or_else(|error| panic!("fixture tool text: {error}"))
    };
    [
        CodexInputItem::AssistantResponseStart,
        CodexInputItem::ToolCall {
            call: call.clone(),
            tool: crate::ToolName::new(name).unwrap_or_else(|| panic!("fixture tool")),
            arguments: bounded(arguments),
        },
        CodexInputItem::ToolOutput {
            call,
            output: bounded(output),
            is_error: false,
        },
    ]
}

fn baseline() -> Vec<CodexInputItem> {
    let mut input = vec![
        system("fixed system instructions").into(),
        system("fixed project instructions").into(),
        user("continue the admitted task").into(),
    ];
    input.extend(tool_exchange(
        "read-0",
        "read_file",
        r#"{"path":"file.rs"}"#,
        "original tool result",
    ));
    input
}

fn full_body(delivery: usize) -> String {
    format!(
        "message {delivery}: continue the admitted task. {}",
        "retained full-body detail. ".repeat(32)
    )
}

fn compact_notification(delivery: usize) -> String {
    // Explicit wire fixture, not a second renderer or the unlanded runtime
    // implementation. The complete body remains in the inbox arm's result.
    format!(
        "Native inbox message\nid: mail-{delivery}\nfrom: fixture-sender\nsource_created_unix_ms: 1760000000123\npreview: continue the admitted task\nFull body: read this id through the bound inbox."
    )
}

fn array(value: &Value, key: &str) -> Vec<Value> {
    value[key]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("missing {key}"))
}

fn input_key(family: ProtocolFamily) -> &'static str {
    match family {
        ProtocolFamily::GoogleGemini => "contents",
        ProtocolFamily::VertexGemini => "contents",
        ProtocolFamily::OpenAiResponses | ProtocolFamily::ChatGptCodex => "input",
        ProtocolFamily::AnthropicMessages
        | ProtocolFamily::OpenAiCompatible
        | ProtocolFamily::BedrockConverse
        | ProtocolFamily::CohereChat
        | ProtocolFamily::VertexAnthropic
        | ProtocolFamily::WatsonxChat => "messages",
    }
}

fn assert_fixed_fields(before: &Value, after: &Value, family: ProtocolFamily) {
    let mut before = before.clone();
    let mut after = after.clone();
    let key = input_key(family);
    before
        .as_object_mut()
        .unwrap_or_else(|| panic!("body"))
        .remove(key);
    after
        .as_object_mut()
        .unwrap_or_else(|| panic!("body"))
        .remove(key);
    assert_eq!(
        before.to_string(),
        after.to_string(),
        "system/tools/model/cache key changed"
    );
}

/// Anthropic permits text after tool results in the same user content array.
/// Compare the prior blocks themselves, without pretending the enclosing
/// array's closing bytes are an unchanged literal JSON prefix.
fn atoms(value: &Value, family: ProtocolFamily) -> Vec<Value> {
    let items = array(value, input_key(family));
    if family != ProtocolFamily::AnthropicMessages {
        return items;
    }
    items
        .iter()
        .flat_map(|message| {
            array(message, "content")
                .into_iter()
                .map(|block| json!({"role": message["role"], "block": block}))
        })
        .collect()
}

/// This comparison deliberately excludes only the existing cache metadata;
/// the separate limit tests compare raw bytes and expose its movement.
fn content_atoms(value: &Value, family: ProtocolFamily) -> Vec<Value> {
    let mut items = atoms(value, family);
    for item in &mut items {
        if family == ProtocolFamily::AnthropicMessages {
            item["block"]
                .as_object_mut()
                .unwrap_or_else(|| panic!("block"))
                .remove("cache_control");
        } else if family == ProtocolFamily::OpenAiCompatible && item["content"].is_array() {
            let parts = array(item, "content");
            assert_eq!(parts.len(), 1);
            assert_eq!(parts[0]["type"], "text");
            assert_eq!(parts[0]["cache_control"]["type"], "ephemeral");
            assert_eq!(
                parts[0]
                    .as_object()
                    .unwrap_or_else(|| panic!("text part"))
                    .len(),
                3
            );
            item["content"] = parts[0]["text"].clone();
        }
    }
    items
}

#[test]
fn n02_both_cached_delivery_arms_keep_prior_content_and_bound_marker_counts() {
    for family in FAMILIES {
        for through_tool in [false, true] {
            let mut input = baseline();
            let mut before = body(&request(family, input.clone(), true));
            for delivery in 1..=3 {
                if through_tool {
                    input.extend(tool_exchange(
                        &format!("mail-read-{delivery}"),
                        "fixture_inbox_read",
                        &format!(r#"{{"id":"mail-{delivery}"}}"#),
                        &full_body(delivery),
                    ));
                } else {
                    input.push(user(&compact_notification(delivery)).into());
                }
                let after = body(&request(family, input.clone(), true));
                assert_fixed_fields(&before, &after, family);
                let old = content_atoms(&before, family);
                let new = content_atoms(&after, family);
                assert_eq!(
                    new[..old.len()],
                    old,
                    "content changed beyond cache metadata"
                );
                let markers = match family {
                    ProtocolFamily::AnthropicMessages => 4,
                    ProtocolFamily::VertexAnthropic => 4,
                    ProtocolFamily::OpenAiCompatible => 4,
                    _ => 0,
                };
                assert_eq!(after.to_string().matches("cache_control").count(), markers);
                before = after;
            }
        }
    }
}

#[test]
fn n02_three_deliveries_preserve_prior_wire_content_without_explicit_breakpoints() {
    for family in FAMILIES {
        let mut native = baseline();
        let mut through_tool = baseline();
        let mut native_before = body(&request(family, native.clone(), false));
        let mut tool_before = native_before.clone();
        for delivery in 1..=3 {
            // The tool arm has a request that obtains the call, as well as the
            // later request consuming its result. Real usage must retain both.
            let tool_call_request = body(&request(family, through_tool.clone(), false));
            assert_eq!(tool_call_request.to_string(), tool_before.to_string());
            native.push(user(&compact_notification(delivery)).into());
            let call_only = tool_exchange(
                &format!("mail-read-{delivery}"),
                "fixture_inbox_read",
                &format!(r#"{{"id":"mail-{delivery}"}}"#),
                &full_body(delivery),
            );
            through_tool.extend(call_only);
            let native_after = body(&request(family, native.clone(), false));
            let tool_after = body(&request(family, through_tool.clone(), false));
            for (before, after) in [(&native_before, &native_after), (&tool_before, &tool_after)] {
                assert_fixed_fields(before, after, family);
                let old = atoms(before, family);
                let new = atoms(after, family);
                assert_eq!(
                    new[..old.len()],
                    old,
                    "prior wire atoms changed for {family:?}"
                );
                assert!(new.len() > old.len());
            }
            let text = native_after.to_string();
            assert!(text.contains(&format!("mail-{delivery}")));
            assert!(!text.contains("retained full-body detail"));
            assert!(tool_after.to_string().contains(&full_body(delivery)));
            // All three original notification ids survive later requests.
            for seen in 1..=delivery {
                assert!(text.contains(&format!("mail-{seen}")));
            }
            native_before = native_after;
            tool_before = tool_after;
        }
    }
}

#[test]
fn n02_responses_prior_wire_items_and_cache_coordinates_stay_byte_stable() {
    for family in [
        ProtocolFamily::OpenAiResponses,
        ProtocolFamily::ChatGptCodex,
    ] {
        let mut input = baseline();
        let mut before = body(&request(family, input.clone(), true));
        for delivery in 1..=3 {
            input.push(user(&compact_notification(delivery)).into());
            let after = body(&request(family, input.clone(), true));
            assert_fixed_fields(&before, &after, family);
            let old = array(&before, "input");
            let new = array(&after, "input");
            for (prior, retained) in old.iter().zip(new.iter()) {
                assert_eq!(prior.to_string(), retained.to_string());
            }
            assert_eq!(new.len(), old.len() + 1);
            assert_eq!(after["prompt_cache_key"], "native-mail-session");
            before = after;
        }
    }
}

#[test]
fn n02_anthropic_cached_wire_limit_is_the_moved_tail_marker_not_rewritten_content() {
    let family = ProtocolFamily::AnthropicMessages;
    let base = baseline();
    let before = body(&request(family, base.clone(), true));
    let before_plain = body(&request(family, base.clone(), false));
    let mut growing = base;
    for delivery in 1..=3 {
        growing.push(user(&compact_notification(delivery)).into());
        let after = body(&request(family, growing.clone(), true));
        assert_fixed_fields(&before, &after, family);
        assert_eq!(after.to_string().matches("cache_control").count(), 4);
        let old_tail = &before["messages"][2]["content"][0];
        let old_after = &after["messages"][2]["content"][0];
        assert_eq!(old_tail["type"], "tool_result");
        assert_eq!(old_tail["cache_control"]["type"], "ephemeral");
        assert!(old_after.get("cache_control").is_none());
        assert_ne!(
            old_tail.to_string(),
            old_after.to_string(),
            "literal cached-wire limit must remain visible"
        );
        if delivery == 1 {
            println!(
                "Anthropic cached-wire limit: old block {old_tail} -> retained block {old_after}"
            );
        }
        let mut without_marker = old_tail.clone();
        without_marker
            .as_object_mut()
            .unwrap_or_else(|| panic!("block"))
            .remove("cache_control");
        assert_eq!(without_marker, *old_after);
        assert_eq!(old_after, &before_plain["messages"][2]["content"][0]);
        assert_eq!(
            after["messages"][2]["content"][delivery]["cache_control"]["type"],
            "ephemeral"
        );
    }
}

#[test]
fn n02_compatible_cached_wire_limit_moves_marked_text_wrappers() {
    let family = ProtocolFamily::OpenAiCompatible;
    let base = baseline();
    let before = body(&request(family, base.clone(), true));
    let mut growing = base;
    growing.push(user(&compact_notification(1)).into());
    let after = body(&request(family, growing, true));
    assert_fixed_fields(&before, &after, family);
    assert_eq!(
        before["messages"][2]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(
        before["messages"][2]["content"][0]["text"],
        after["messages"][2]["content"]
    );
    assert!(after["messages"][2]["content"].is_string());
    assert_ne!(
        before["messages"][2].to_string(),
        after["messages"][2].to_string()
    );
    println!(
        "Compatible cached-wire limit: old message {} -> retained message {}",
        before["messages"][2], after["messages"][2]
    );
    assert_eq!(before.to_string().matches("cache_control").count(), 4);
    assert_eq!(after.to_string().matches("cache_control").count(), 4);
    assert_eq!(
        after["messages"][5]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
}

fn usage(
    family: ProtocolFamily,
    frame: &Value,
) -> Result<Option<TokenUsage>, UsageNormalizationError> {
    match family {
        ProtocolFamily::GoogleGemini => crate::usage::normalize_gemini_usage(frame),
        ProtocolFamily::VertexGemini => crate::usage::normalize_gemini_usage(frame),
        ProtocolFamily::OpenAiResponses => {
            normalize_responses_usage(frame, UsageRoute::OpenAiResponses)
        }
        ProtocolFamily::ChatGptCodex => normalize_responses_usage(frame, UsageRoute::ChatGptCodex),
        ProtocolFamily::AnthropicMessages => normalize_anthropic_usage(frame),
        ProtocolFamily::OpenAiCompatible => normalize_compatible_usage(frame),
        ProtocolFamily::BedrockConverse => crate::usage::normalize_bedrock_usage(frame),
        ProtocolFamily::CohereChat => crate::usage::normalize_cohere_usage(frame),
        ProtocolFamily::VertexAnthropic => normalize_anthropic_usage(frame),
        ProtocolFamily::WatsonxChat => normalize_compatible_usage(frame),
    }
}

#[test]
fn n02_cache_requests_supply_no_usage_evidence_and_synthetic_reports_keep_provenance(
) -> Result<(), Box<dyn std::error::Error>> {
    for family in FAMILIES {
        let mut input = baseline();
        for delivery in 1..=3 {
            input.push(user(&compact_notification(delivery)).into());
            let emitted = body(&request(family, input.clone(), true));
            assert!(
                usage(family, &emitted)?.is_none(),
                "request markers are not reported usage"
            );
            let frame = match family {
                ProtocolFamily::AnthropicMessages => json!({"usage":{"input_tokens":10,
                    "cache_read_input_tokens":100,"cache_creation_input_tokens":2}}),
                ProtocolFamily::VertexAnthropic => json!({"usage":{"input_tokens":10,
                    "cache_read_input_tokens":100,"cache_creation_input_tokens":2}}),
                ProtocolFamily::OpenAiCompatible => json!({"usage":{"prompt_tokens":112,
                    "prompt_tokens_details":{"cached_tokens":100,"cache_write_tokens":2}}}),
                _ => json!({"usage":{"input_tokens":112,
                    "input_tokens_details":{"cached_tokens":100,"cache_write_tokens":2}}}),
            };
            let normalized = usage(family, &frame)?.ok_or("synthetic report absent")?;
            assert_eq!(normalized.cache_read().value(), Some(100));
            assert_eq!(normalized.cache_write().value(), Some(2));
            assert_eq!(
                normalized.cache_read().provenance(),
                Some(&UsageProvenance::ProviderReported)
            );
            assert_eq!(
                normalized.cache_write().provenance(),
                Some(&UsageProvenance::ProviderReported)
            );
            assert_eq!(normalized.input_total().value(), Some(112));
            assert_eq!(normalized.input_fresh().value(), Some(10));
            let (derived, rule) = if family == ProtocolFamily::AnthropicMessages {
                (
                    normalized.input_total(),
                    UsageDerivation::InclusiveInputFromPartition,
                )
            } else {
                (
                    normalized.input_fresh(),
                    UsageDerivation::FreshFromInclusiveInput,
                )
            };
            assert_eq!(
                derived.provenance(),
                Some(&UsageProvenance::ProviderDerived { rule })
            );
            let partial = match family {
                ProtocolFamily::OpenAiCompatible => json!({"usage":{"prompt_tokens":112}}),
                _ => json!({"usage":{"input_tokens":112}}),
            };
            let partial = usage(family, &partial)?.ok_or("partial absent")?;
            assert!(partial.cache_read().is_absent());
            assert!(partial.cache_write().is_absent());
            assert!(partial.cache_read().provenance().is_none());
        }
    }
    Ok(())
}
