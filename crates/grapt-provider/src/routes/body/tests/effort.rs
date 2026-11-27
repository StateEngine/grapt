//! Effort changes preserve previous wire input and the top-level baseline.
use super::*;
use crate::{EffortPolicy, ReasoningEffort as Effort};
use serde_json::json;

fn history() -> Vec<CodexInputItem> {
    vec![
        system("stable policy").into(),
        CodexInputItem::EffortUpdate(Effort::Medium),
        user("first").into(),
        InputMessage {
            role: MessageRole::Assistant,
            content: "answer".into(),
        }
        .into(),
        CodexInputItem::EffortUpdate(Effort::High),
        user("second").into(),
    ]
}
fn codex(input: Vec<CodexInputItem>, current: &str, updates: bool) -> ProviderRequest {
    ProviderRequest::ChatGptCodex(ChatGptCodexRequest {
        model: model_variant(
            "openai",
            "account-selected-model",
            grapt_types::ModelVariantId::new(current),
        ),
        input,
        prompt_cache_key: None,
        tools: vec![],
        web_search: None,
        tool_choice: None,
        reasoning: crate::CodexReasoning {
            effort: EffortPolicy {
                mode: if updates {
                    crate::EffortMode::Chronological
                } else {
                    crate::EffortMode::Request
                },
                default: Some(Effort::Medium),
            },
            ..Default::default()
        },
        turn: Default::default(),
    })
}

#[test]
fn responses_effort_controls_preserve_prefix_and_survive_in_band_compaction() {
    let input = history();
    let first = body(&codex(input[..3].to_vec(), "medium", true));
    let next = body(&codex(input.clone(), "high", true));
    assert_eq!(first["reasoning"]["effort"], "medium");
    assert_eq!(next["reasoning"], first["reasoning"]);
    let prefix = first["input"].as_array().unwrap_or_else(|| panic!("input"));
    let later = next["input"].as_array().unwrap_or_else(|| panic!("input"));
    assert_eq!(&later[..prefix.len()], prefix);
    assert_eq!(
        later[3],
        json!({"type":"configuration_update","reasoning":{"effort":"high"}})
    );
    assert_eq!(later[4]["role"], "user");
    let mut compacting = input;
    compacting.push(CodexInputItem::CompactionTrigger);
    let compacting = body(&codex(compacting, "high", true));
    assert_eq!(compacting["input"][3], later[3]);
    assert_eq!(compacting["input"][5], json!({"type":"compaction_trigger"}));
    // A fresh retained context pins the selected value, not the retired one.
    let fresh = body(&codex(
        vec![
            CodexInputItem::EffortUpdate(Effort::High),
            user("retained").into(),
        ],
        "high",
        true,
    ));
    assert_eq!(fresh["reasoning"]["effort"], "high");
    assert_eq!(fresh["input"].as_array().map(Vec::len), Some(1));
}

#[test]
fn unsupported_routes_and_reverted_settings_use_current_request_effort() {
    for (current, updates) in [("high", false), ("low", true)] {
        let request = codex(history(), current, updates);
        assert!(crate::routes::request_validation::validate(&request).is_ok());
        let body = body(&request);
        assert_eq!(body["reasoning"]["effort"], current);
        assert!(body["input"]
            .as_array()
            .unwrap_or_else(|| panic!("input"))
            .iter()
            .all(|item| item["type"] != "configuration_update"));
    }
    let mut adjacent = history();
    adjacent.insert(5, CodexInputItem::EffortUpdate(Effort::Max));
    let body = body(&codex(adjacent, "max", true));
    assert_eq!(body["input"][3]["reasoning"]["effort"], "max");
    assert_eq!(body["input"][4]["role"], "user");
}

#[test]
fn native_effort_only_system_control_precedes_new_input_and_has_no_cache_marker() {
    let native = |messages| {
        ProviderRequest::AnthropicMessages(AnthropicMessagesRequest {
            web_search: None,
            uncached_system_suffix: None,
            effort: EffortPolicy {
                mode: crate::EffortMode::Chronological,
                default: Some(Effort::Medium),
            },
            model: model_variant(
                "anthropic",
                "claude-opus-5-5",
                grapt_types::ModelVariantId::new("high"),
            ),
            messages,
            max_tokens: bound(8192),
            thinking: Some(ThinkingConfig::Adaptive),
            instruction_mode: crate::AnthropicInstructionMode::Epochs,
            prompt_cache: crate::AnthropicPromptCache::StablePrefix,
            tools: vec![],
            tool_choice: None,
        })
    };
    let body = body(&native(history()));
    assert_eq!(body["output_config"], json!({"effort":"medium"}));
    let messages = body["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("messages"));
    assert_eq!(
        messages[2],
        json!({"role":"system","content":[],"output_config":{"effort":"high"}})
    );
    assert_eq!(messages[3]["role"], "user");
    assert_eq!(
        messages[0]["content"][0]["cache_control"],
        json!({"type":"ephemeral","ttl":"1h"})
    );
    assert_eq!(
        messages[3]["content"][0]["cache_control"],
        json!({"type":"ephemeral","ttl":"1h"})
    );
}

#[test]
fn advertised_native_model_uses_request_effort_without_claiming_a_default_or_inline_support() {
    let request = |variant| {
        ProviderRequest::AnthropicMessages(AnthropicMessagesRequest {
            web_search: None,
            uncached_system_suffix: None,
            effort: EffortPolicy {
                mode: crate::EffortMode::Request,
                default: None,
            },
            model: model_variant("anthropic", "advertised-native", variant),
            messages: history(),
            max_tokens: bound(8192),
            thinking: Some(ThinkingConfig::Adaptive),
            instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
            prompt_cache: crate::AnthropicPromptCache::StablePrefix,
            tools: vec![],
            tool_choice: None,
        })
    };
    let explicit = request(grapt_types::ModelVariantId::new("high"));
    assert!(crate::routes::request_validation::validate(&explicit).is_ok());
    let explicit = body(&explicit);
    assert_eq!(explicit["output_config"]["effort"], "high");
    assert!(explicit["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("messages"))
        .iter()
        .all(|message| message["role"] != "system"));
    assert!(body(&request(None)).get("output_config").is_none());
}

#[test]
fn opaque_compaction_reasserts_selected_effort_before_the_next_user() {
    let selected = model("openai", "account-selected-model");
    let state = json!({"kind":"openai-compaction-v1", "binding":vec![0u8;32],
        "item":{"type":"compaction","id":"cmp_effort","encrypted_content":"COMPACTED"}});
    let replay = crate::ProviderReplay::restore(
        selected.provider().clone(),
        selected.model().clone(),
        None,
        &state.to_string(),
    )
    .unwrap_or_else(|error| panic!("compaction fixture: {error}"));
    let request = codex(
        vec![
            CodexInputItem::ProviderReplay(replay),
            CodexInputItem::EffortUpdate(Effort::High),
            user("continue").into(),
        ],
        "high",
        true,
    );
    assert!(crate::routes::request_validation::validate(&request).is_ok());
    let body = body(&request);
    assert_eq!(body["reasoning"]["effort"], "high");
    assert_eq!(body["input"][0]["type"], "compaction");
    assert_eq!(
        body["input"][1],
        json!({"type":"configuration_update","reasoning":{"effort":"high"}})
    );
    assert_eq!(body["input"][2]["role"], "user");
}
