//! Bounded pre-transport validation of concrete request fields.

use crate::{CodexInputItem, ProtocolFamily, ProviderRequest, ToolChoice};

use crate::tool_eligibility::MAX_REQUEST_TOOLS;

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(crate) fn validate(request: &ProviderRequest) -> Result<(), &'static str> {
    if request.family() == ProtocolFamily::BedrockConverse && request.model().variant().is_some() {
        return Err("Bedrock effort variants are not yet supported");
    }
    if let Some(effort) = request.model().variant() {
        let common = matches!(effort.as_str(), "low" | "medium" | "high" | "xhigh" | "max");
        let openai =
            !request.family().is_anthropic() && matches!(effort.as_str(), "none" | "minimal");
        if !common && !openai {
            return Err("selected reasoning effort is unsupported by this provider family");
        }
    }
    let items = match request {
        ProviderRequest::OpenAiResponses(request) => &request.input,
        ProviderRequest::ChatGptCodex(request) => &request.input,
        ProviderRequest::AnthropicMessages(request) | ProviderRequest::VertexAnthropic(request) => {
            if request.thinking.as_ref().is_some_and(|thinking| {
                matches!(thinking, crate::ThinkingConfig::Enabled { budget_tokens }
                    if *budget_tokens < 1024 || *budget_tokens >= request.max_tokens.value())
            }) {
                return Err("Anthropic thinking budget must be at least 1024 and below max_tokens");
            }
            if request.thinking.is_some()
                && matches!(
                    request.tool_choice,
                    Some(ToolChoice::Required | ToolChoice::Named { .. })
                )
            {
                return Err("Anthropic thinking does not support forced tool choice");
            }
            &request.messages
        }
        ProviderRequest::OpenAiCompatible(request) | ProviderRequest::WatsonxChat(request) => {
            &request.messages
        }
        ProviderRequest::BedrockConverse(request) => &request.messages,
        ProviderRequest::GoogleGemini(request) | ProviderRequest::VertexGemini(request) => {
            &request.messages
        }
        ProviderRequest::CohereChat(request) => &request.messages,
    };
    if request
        .tools()
        .len()
        .saturating_add(usize::from(request.web_search().is_some()))
        .saturating_add(usize::from(request.anthropic_web_search().is_some()))
        > MAX_REQUEST_TOOLS
    {
        return Err("provider request exceeds its tool count bound");
    }
    super::response_groups::validate(items)?;
    if (request.web_search().is_some() || request.anthropic_web_search().is_some())
        && (request.tools().iter().any(|t| t.name == "web_search")
            || matches!(request.tool_choice(), Some(ToolChoice::Named {tool}) if tool == "web_search"))
    {
        return Err("native web_search is reserved and cannot be selected as a function");
    }
    let mut image_count = 0usize;
    for item in items {
        match item {
            CodexInputItem::AssistantResponseStart => {}
            CodexInputItem::CompatibleReasoning(_) => {
                if !matches!(request, ProviderRequest::OpenAiCompatible(r)
                    if r.reasoning_replay == crate::CompatibleReasoningReplay::ReasoningContent)
                {
                    return Err("compatible reasoning requires its declared endpoint capability");
                }
            }
            CodexInputItem::EffortUpdate(value) => {
                let valid = match request {
                    ProviderRequest::ChatGptCodex(_) => true,
                    ProviderRequest::AnthropicMessages(_) | ProviderRequest::VertexAnthropic(_) => {
                        value.native()
                    }
                    _ => false,
                };
                if !valid {
                    return Err("effort update requires a supported protocol and level");
                }
            }
            CodexInputItem::CompactionTrigger => {
                if !matches!(
                    request.family(),
                    ProtocolFamily::OpenAiResponses | ProtocolFamily::ChatGptCodex
                ) {
                    return Err("a compaction trigger requires the OpenAI Responses protocol");
                }
            }
            CodexInputItem::ProviderReplay(replay) => {
                let protocol = if replay.is_bedrock_thinking() {
                    request.family() == ProtocolFamily::BedrockConverse
                } else if replay.is_gemini() {
                    matches!(
                        request.family(),
                        ProtocolFamily::GoogleGemini | ProtocolFamily::VertexGemini
                    )
                } else if replay.is_anthropic_search() || replay.is_anthropic_text() {
                    request.family().is_anthropic()
                } else if replay.is_responses_reasoning()
                    || replay.is_responses_compaction()
                    || replay.native_search_call().is_some()
                {
                    matches!(
                        request.family(),
                        ProtocolFamily::OpenAiResponses | ProtocolFamily::ChatGptCodex
                    )
                } else {
                    request.family().is_anthropic()
                };
                let target_matches = if replay.is_responses_compaction() {
                    replay.matches_compaction(request.model(), crate::effort::policy(request))
                } else {
                    replay.matches_effort(request.model(), crate::effort::policy(request))
                };
                if !protocol || !target_matches {
                    return Err("provider replay does not match the selected protocol and target");
                }
            }
            CodexInputItem::Message(_)
            | CodexInputItem::ToolCall { .. }
            | CodexInputItem::TextToolCall { .. }
            | CodexInputItem::ToolOutput { .. } => {}
            CodexInputItem::ImageUser { images, .. }
            | CodexInputItem::ImageToolOutput { images, .. } => {
                image_count = image_count.saturating_add(images.items().len());
                if image_count > crate::MAX_REQUEST_IMAGES {
                    return Err("provider request exceeds its image count bound");
                }
            }
        }
    }
    // A definition the provider cannot carry at all is excluded by the product,
    // through `tool_eligibility`, with a notice naming it: one bad definition
    // must not brick a long-lived session. Size is capacity and lives there.
    //
    // Shape is not capacity. A schema that is not a JSON object would be sent
    // as a malformed request, and the product's eligibility pass should already
    // have excluded it, so this stays as the fail-closed invariant for
    // anything that reaches staging anyway.
    for tool in request.tools() {
        let schema: serde_json::Value = serde_json::from_str(&tool.input_schema)
            .map_err(|_| "provider tool schema is not JSON")?;
        if !schema.is_object() {
            return Err("provider tool schema must be an object");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::fixtures::{adapter, anthropic_request, input, responses_request, user};

    #[test]
    fn an_unusable_tool_definition_is_excluded_rather_than_refusing_the_request() {
        use crate::tool_eligibility::{eligibility, Eligibility, ExclusionReason};

        for (description, reason) in [
            (Some(String::new()), ExclusionReason::DescriptionEmpty),
            (
                Some("x".repeat(crate::tool_eligibility::MAX_TOOL_DESCRIPTION_BYTES + 1)),
                ExclusionReason::DescriptionTooLarge,
            ),
        ] {
            let tool = crate::ToolDefinition {
                input_format: crate::ToolInputFormat::Json,
                name: "read".into(),
                description: description.clone(),
                input_schema: "{}".into(),
            };
            assert_eq!(eligibility(&tool), Eligibility::Excluded(reason));
            // The request still validates: one unusable definition is left out
            // with a notice, not turned into a turn-ending refusal.
            let mut request = responses_request();
            let ProviderRequest::OpenAiResponses(responses) = &mut request else {
                panic!("family");
            };
            responses.tools.push(tool);
            assert!(validate(&request).is_ok(), "description {description:?}");
        }

        let usable = crate::ToolDefinition {
            input_format: crate::ToolInputFormat::Json,
            name: "read".into(),
            description: Some("x".repeat(crate::tool_eligibility::MAX_TOOL_DESCRIPTION_BYTES)),
            input_schema: "{}".into(),
        };
        assert_eq!(eligibility(&usable), Eligibility::Eligible);
        let mut request = responses_request();
        let ProviderRequest::OpenAiResponses(responses) = &mut request else {
            panic!("family");
        };
        responses.tools.push(usable);
        assert!(validate(&request).is_ok());
    }

    #[test]
    fn pc_valid_lower_level_thinking_is_staged_and_encoded() {
        let mut request = anthropic_request();
        let ProviderRequest::AnthropicMessages(anthropic) = &mut request else {
            panic!("family");
        };
        anthropic.max_tokens = crate::MaxOutputTokens::new(4096).unwrap_or_else(|| panic!("bound"));
        anthropic.thinking = Some(crate::ThinkingConfig::Enabled {
            budget_tokens: 2048,
        });
        let body = crate::routes::body::anthropic_body(anthropic)
            .unwrap_or_else(|error| panic!("body: {error}"));
        assert_eq!(
            body["thinking"],
            serde_json::json!({"type":"enabled", "budget_tokens":2048})
        );
        assert!(adapter(1)
            .stage(input(
                "pc-valid-thinking",
                "http://127.0.0.1:1",
                ProtocolFamily::AnthropicMessages,
                request
            ))
            .is_ok());
    }

    #[test]
    fn adaptive_thinking_has_summary_display_and_no_manual_budget() {
        let mut request = anthropic_request();
        let ProviderRequest::AnthropicMessages(anthropic) = &mut request else {
            panic!("family");
        };
        anthropic.thinking = Some(crate::ThinkingConfig::Adaptive);
        let body = crate::routes::body::anthropic_body(anthropic)
            .unwrap_or_else(|error| panic!("body: {error}"));
        assert_eq!(
            body["thinking"],
            serde_json::json!({"type":"adaptive","display":"summarized"})
        );
        assert_eq!(validate(&request), Ok(()));
        assert!(adapter(1)
            .stage(input(
                "adaptive-thinking",
                "http://127.0.0.1:1",
                ProtocolFamily::AnthropicMessages,
                request
            ))
            .is_ok());
    }

    #[test]
    fn pc_request_bounds_and_thinking_budget_validate_before_transport() {
        let provider = adapter(1);
        for budget_tokens in [0, 1, 1023, 4096, 4097, u32::MAX] {
            let mut request = anthropic_request();
            let ProviderRequest::AnthropicMessages(anthropic) = &mut request else {
                panic!("family");
            };
            anthropic.max_tokens =
                crate::MaxOutputTokens::new(4096).unwrap_or_else(|| panic!("bound"));
            anthropic.thinking = Some(crate::ThinkingConfig::Enabled { budget_tokens });
            assert_eq!(
                validate(&request),
                Err("Anthropic thinking budget must be at least 1024 and below max_tokens"),
                "budget {budget_tokens}"
            );
            assert!(
                provider
                    .stage(input(
                        "attempt_pc_thinking",
                        "http://127.0.0.1:1",
                        ProtocolFamily::AnthropicMessages,
                        request
                    ))
                    .is_err(),
                "budget {budget_tokens}"
            );
        }
        // Growth alone no longer refuses a request: a long session keeps
        // sending, and the count that used to be the ceiling now stages.
        let mut request = responses_request();
        let ProviderRequest::OpenAiResponses(responses) = &mut request else {
            panic!("family");
        };
        responses.input = vec![user("x").into(); 4_097];
        assert!(provider
            .stage(input(
                "attempt_pc_items",
                "http://127.0.0.1:1",
                ProtocolFamily::OpenAiResponses,
                request
            ))
            .is_ok());
        assert!(validate(&responses_request()).is_ok());
        assert!(validate(&anthropic_request()).is_ok());
    }

    #[test]
    fn pc_thinking_tool_choices_preserve_valid_offers_and_refuse_forced_choices() {
        for budget_tokens in [1024, 2048, 4095] {
            for choice in [
                None,
                Some(ToolChoice::Auto),
                Some(ToolChoice::None),
                Some(ToolChoice::Required),
                Some(ToolChoice::Named {
                    tool: "lookup".into(),
                }),
            ] {
                for thinking_enabled in [false, true] {
                    let mut request = anthropic_request();
                    let ProviderRequest::AnthropicMessages(anthropic) = &mut request else {
                        panic!("family");
                    };
                    anthropic.max_tokens =
                        crate::MaxOutputTokens::new(4096).unwrap_or_else(|| panic!("bound"));
                    anthropic.thinking = thinking_enabled
                        .then_some(crate::ThinkingConfig::Enabled { budget_tokens });
                    anthropic.tools = vec![crate::ToolDefinition {
                        input_format: crate::ToolInputFormat::Json,
                        name: "lookup".into(),
                        description: None,
                        input_schema: r#"{"type":"object","properties":{}}"#.into(),
                    }];
                    anthropic.tool_choice = choice.clone();
                    let forced = matches!(
                        choice,
                        Some(ToolChoice::Required | ToolChoice::Named { .. })
                    );
                    let allowed = !thinking_enabled || !forced;
                    if allowed {
                        let body = crate::routes::body::anthropic_body(anthropic)
                            .unwrap_or_else(|error| panic!("body: {error}"));
                        assert_eq!(
                            body["tools"],
                            serde_json::json!([{
                                "name":"lookup", "input_schema":{"type":"object","properties":{}}
                            }])
                        );
                        if thinking_enabled {
                            assert_eq!(body["thinking"]["budget_tokens"], budget_tokens);
                        } else {
                            assert!(body.get("thinking").is_none());
                        }
                        let expected = match &choice {
                            None => serde_json::Value::Null,
                            Some(ToolChoice::Auto) => serde_json::json!({"type":"auto"}),
                            Some(ToolChoice::None) => serde_json::json!({"type":"none"}),
                            Some(ToolChoice::Required) => serde_json::json!({"type":"any"}),
                            Some(ToolChoice::Named { tool }) => {
                                serde_json::json!({"type":"tool","name":tool})
                            }
                        };
                        assert_eq!(body["tool_choice"], expected);
                    } else {
                        assert_eq!(
                            validate(&request),
                            Err("Anthropic thinking does not support forced tool choice")
                        );
                    }
                    assert_eq!(
                        adapter(1)
                            .stage(input(
                                "pc-thinking-tools",
                                "http://127.0.0.1:1",
                                ProtocolFamily::AnthropicMessages,
                                request,
                            ))
                            .is_ok(),
                        allowed,
                        "budget {budget_tokens}, choice {choice:?}, thinking {thinking_enabled}"
                    );
                }
            }
        }
    }
}
