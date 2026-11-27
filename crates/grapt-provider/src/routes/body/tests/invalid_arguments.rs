//! A history call whose arguments are not a JSON object reaches every route as
//! the invalid-arguments marker, while valid arguments reach it unchanged.
use super::*;
use serde_json::Value;

type Built<T> = Result<T, Box<dyn std::error::Error>>;

fn marker() -> Value {
    serde_json::json!({ "_invalid_arguments_truncated": true })
}

fn call(arguments: &str) -> Built<CodexInputItem> {
    Ok(CodexInputItem::ToolCall {
        call: ToolCallId::new("call_edit").ok_or("call")?,
        tool: ToolName::new("edit").ok_or("tool")?,
        arguments: CodexToolText::try_new(arguments.to_owned())?,
    })
}

fn text_call(input: &str) -> Built<CodexInputItem> {
    Ok(CodexInputItem::TextToolCall {
        call: ToolCallId::new("call_edit").ok_or("call")?,
        tool: ToolName::new("apply_patch").ok_or("tool")?,
        input: CodexToolText::try_new(input.to_owned())?,
    })
}

fn history(item: CodexInputItem) -> Built<Vec<CodexInputItem>> {
    let call = ToolCallId::new("call_edit").ok_or("call")?;
    Ok(vec![
        user("edit it").into(),
        CodexInputItem::AssistantResponseStart,
        item,
        CodexInputItem::ToolOutput {
            call,
            output: CodexToolText::try_new("tool input is invalid JSON".into())?,
            is_error: true,
        },
        user("try again").into(),
    ])
}

fn requests(item: CodexInputItem) -> Built<Vec<ProviderRequest>> {
    let items = history(item)?;
    Ok(vec![
        ProviderRequest::OpenAiResponses(OpenAiResponsesRequest {
            model: model("openai", "gpt-5"),
            input: items.clone(),
            max_output_tokens: None,
            prompt_cache_key: None,
            tools: Vec::new(),
            web_search: None,
            tool_choice: None,
        }),
        ProviderRequest::OpenAiCompatible(OpenAiCompatibleRequest {
            reasoning_replay: Default::default(),
            prompt_cache: crate::CompatiblePromptCache::Off,
            model: model("local", "served-model"),
            messages: items.clone(),
            max_tokens: None,
            tools: Vec::new(),
            tool_choice: None,
            routing_session: None,
        }),
        ProviderRequest::AnthropicMessages(AnthropicMessagesRequest {
            web_search: None,
            uncached_system_suffix: None,
            effort: crate::EffortPolicy::default(),
            model: model("anthropic", "claude-opus-5"),
            messages: items.clone(),
            max_tokens: bound(16),
            instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
            thinking: None,
            prompt_cache: crate::AnthropicPromptCache::Off,
            tools: Vec::new(),
            tool_choice: None,
        }),
        ProviderRequest::BedrockConverse(crate::BedrockConverseRequest {
            model: model("bedrock", "served-model"),
            messages: items.clone(),
            max_tokens: bound(16),
            tools: Vec::new(),
            tool_choice: None,
        }),
        ProviderRequest::GoogleGemini(crate::GoogleGeminiRequest {
            model: model("google", "gemini-2.5-pro"),
            messages: items.clone(),
            max_tokens: None,
            tools: Vec::new(),
            tool_choice: None,
        }),
        ProviderRequest::CohereChat(crate::CohereChatRequest {
            model: model("cohere", "command-a"),
            messages: items,
            max_tokens: None,
            tools: Vec::new(),
            tool_choice: None,
        }),
    ])
}

/// The first projected call arguments: a JSON string or an object.
fn projected(value: &Value) -> Option<&Value> {
    match value {
        Value::Object(fields) => fields
            .iter()
            .find(|(key, value)| {
                matches!(key.as_str(), "arguments" | "args" | "input")
                    && (value.is_string() || value.is_object())
            })
            .map(|(_, value)| value)
            .or_else(|| fields.values().find_map(projected)),
        Value::Array(items) => items.iter().find_map(projected),
        _ => None,
    }
}

#[test]
fn invalid_history_arguments_project_as_an_object_on_every_route() -> Built<()> {
    // Truncated JSON, valid JSON that is not an object, and no text at all.
    for arguments in [r#"{"new_text":"abc"#, "[1]", r#""x""#, "5", ""] {
        for request in requests(call(arguments)?)? {
            let wire = body(&request);
            let projected = projected(&wire).ok_or("projected arguments")?;
            let value = match projected {
                Value::String(text) => serde_json::from_str(text)?,
                value => value.clone(),
            };
            assert_eq!(value, marker(), "{arguments:?} {request:?}");
        }
    }
    Ok(())
}

#[test]
fn a_text_call_keeps_its_text_input_on_every_route() -> Built<()> {
    let input = "*** Begin Patch\n*** End Patch";
    for request in requests(text_call(input)?)? {
        let wire = body(&request);
        match projected(&wire).ok_or("projected input")? {
            // The Responses route sends the grammar call's text as itself.
            Value::String(text) if text == input => (),
            Value::String(text) => assert_eq!(
                serde_json::from_str::<Value>(text)?,
                serde_json::json!({ "input": input }),
                "{request:?}"
            ),
            value => assert_eq!(value, &serde_json::json!({ "input": input }), "{request:?}"),
        }
    }
    Ok(())
}

#[test]
fn valid_history_arguments_project_unchanged_on_every_route() -> Built<()> {
    let text = r#"{"path":"a.txt", "new_text":"abc"}"#;
    for request in requests(call(text)?)? {
        let wire = body(&request);
        match projected(&wire).ok_or("projected arguments")? {
            Value::String(projected) if !matches!(request, ProviderRequest::CohereChat(_)) => {
                assert_eq!(projected, text, "{request:?}");
            }
            Value::String(projected) => {
                assert_eq!(
                    serde_json::from_str::<Value>(projected)?,
                    serde_json::from_str::<Value>(text)?
                );
            }
            value => assert_eq!(value, &serde_json::from_str::<Value>(text)?, "{request:?}"),
        }
    }
    Ok(())
}

/// A Gemini history whose call kept the provider's native part.
fn gemini_with_native(arguments: &str) -> Built<crate::GoogleGeminiRequest> {
    let part = serde_json::json!({
        "kind": "gemini-part-v1",
        "binding": vec![0u8; 32],
        "item": {
            "functionCall": {"id": "call_edit", "name": "edit", "args": {"new_text": "abc"}},
            "thoughtSignature": "native-signature",
        },
    });
    let replay = crate::replay::ProviderReplay::restore(
        grapt_types::ProviderId::new("google").ok_or("provider")?,
        grapt_types::ModelId::new("gemini-2.5-pro").ok_or("model")?,
        None,
        &part.to_string(),
    )?;
    let mut messages = history(call(arguments)?)?;
    messages.insert(2, CodexInputItem::ProviderReplay(replay));
    Ok(crate::GoogleGeminiRequest {
        model: model("google", "gemini-2.5-pro"),
        messages,
        max_tokens: None,
        tools: Vec::new(),
        tool_choice: None,
    })
}

#[test]
fn gemini_replays_the_native_call_when_the_typed_arguments_were_replaced() -> Built<()> {
    let wire = crate::gemini::body(&gemini_with_native(r#"{"new_text":"abc"#)?)?;
    let part = projected(&wire).ok_or("projected args")?;
    assert_eq!(part, &serde_json::json!({"new_text": "abc"}));
    assert!(wire.to_string().contains("native-signature"), "{wire}");
    // Valid typed arguments must still agree with the native part exactly.
    assert!(crate::gemini::body(&gemini_with_native(r#"{"new_text":"other"}"#)?).is_err());
    assert!(crate::gemini::body(&gemini_with_native(r#"{"new_text":"abc"}"#)?).is_ok());
    Ok(())
}
