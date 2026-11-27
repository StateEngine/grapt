use super::*;

fn request(family: crate::ProtocolFamily, effort: Option<&str>) -> Result<ProviderRequest, String> {
    let variant = effort
        .map(|value| grapt_types::ModelVariantId::new(value).ok_or("effort"))
        .transpose()?;
    let model = model_variant("fixture", "reasoning-model", variant);
    Ok(match family {
        crate::ProtocolFamily::BedrockConverse => {
            return Err("native Bedrock effort is not supported".into())
        }
        crate::ProtocolFamily::GoogleGemini => {
            return Err("Gemini does not advertise effort variants".into());
        }
        crate::ProtocolFamily::CohereChat => {
            return Err("Cohere does not advertise effort variants".into())
        }
        crate::ProtocolFamily::VertexGemini => {
            return Err("Gemini does not advertise effort variants".into());
        }
        crate::ProtocolFamily::OpenAiResponses => {
            ProviderRequest::OpenAiResponses(OpenAiResponsesRequest {
                model,
                input: Vec::new(),
                prompt_cache_key: None,
                max_output_tokens: None,
                tools: Vec::new(),
                web_search: None,
                tool_choice: None,
            })
        }
        crate::ProtocolFamily::ChatGptCodex => ProviderRequest::ChatGptCodex(ChatGptCodexRequest {
            model,
            input: Vec::new(),
            prompt_cache_key: None,
            tools: Vec::new(),
            web_search: None,
            tool_choice: None,
            reasoning: Default::default(),
            turn: Default::default(),
        }),
        crate::ProtocolFamily::AnthropicMessages => {
            ProviderRequest::AnthropicMessages(AnthropicMessagesRequest {
                web_search: None,
                uncached_system_suffix: None,
                effort: crate::EffortPolicy {
                    mode: crate::EffortMode::Request,
                    default: None,
                },
                instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
                model,
                messages: Vec::new(),
                max_tokens: MaxOutputTokens::new(8192).ok_or("bound")?,
                thinking: Some(ThinkingConfig::Adaptive),
                prompt_cache: crate::AnthropicPromptCache::Off,
                tools: Vec::new(),
                tool_choice: None,
            })
        }
        crate::ProtocolFamily::VertexAnthropic => {
            ProviderRequest::VertexAnthropic(AnthropicMessagesRequest {
                web_search: None,
                uncached_system_suffix: None,
                effort: crate::EffortPolicy {
                    mode: crate::EffortMode::Request,
                    default: None,
                },
                instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
                model,
                messages: Vec::new(),
                max_tokens: MaxOutputTokens::new(8192).ok_or("bound")?,
                thinking: Some(ThinkingConfig::Adaptive),
                prompt_cache: crate::AnthropicPromptCache::Off,
                tools: Vec::new(),
                tool_choice: None,
            })
        }
        crate::ProtocolFamily::OpenAiCompatible => {
            ProviderRequest::OpenAiCompatible(OpenAiCompatibleRequest {
                reasoning_replay: Default::default(),
                model,
                messages: Vec::new(),
                max_tokens: None,
                prompt_cache: crate::CompatiblePromptCache::Off,
                tools: Vec::new(),
                tool_choice: None,
                routing_session: None,
            })
        }
        crate::ProtocolFamily::WatsonxChat => {
            ProviderRequest::WatsonxChat(OpenAiCompatibleRequest {
                reasoning_replay: Default::default(),
                model,
                messages: Vec::new(),
                max_tokens: None,
                prompt_cache: crate::CompatiblePromptCache::Off,
                tools: Vec::new(),
                tool_choice: None,
                routing_session: None,
            })
        }
    })
}

#[test]
fn effort_uses_the_selected_familys_field_and_unsupported_values_refuse() -> Result<(), String> {
    use crate::ProtocolFamily::*;
    for family in [
        OpenAiResponses,
        ChatGptCodex,
        AnthropicMessages,
        OpenAiCompatible,
        WatsonxChat,
    ] {
        let request = request(family, Some("high"))?;
        crate::routes::request_validation::validate(&request)?;
        let body = match &request {
            ProviderRequest::GoogleGemini(r) | ProviderRequest::VertexGemini(r) => {
                crate::gemini::body(r)?
            }
            ProviderRequest::OpenAiResponses(r) => super::super::openai_responses_body(r)?,
            ProviderRequest::ChatGptCodex(r) => super::super::codex_body(r)?,
            ProviderRequest::AnthropicMessages(r) | ProviderRequest::VertexAnthropic(r) => {
                super::super::anthropic_body(r)?
            }
            ProviderRequest::OpenAiCompatible(r) => {
                super::super::compatible_body(r, crate::CompatibleProtocol::OpenAi)?
            }
            ProviderRequest::BedrockConverse(r) => crate::routes::bedrock::body(r)?,
            ProviderRequest::CohereChat(r) => crate::cohere::body(r)?,
            ProviderRequest::WatsonxChat(r) => crate::watsonx::body(
                r,
                &grapt_types::WatsonxProjectId::new("fixture-project").ok_or("fixture project")?,
            )?,
        };
        match family {
            BedrockConverse => return Err("native Bedrock effort is not supported".into()),
            GoogleGemini | VertexGemini => {
                return Err("Gemini does not advertise effort variants".into())
            }
            CohereChat => return Err("Cohere does not advertise effort variants".into()),
            OpenAiResponses | ChatGptCodex => {
                assert_eq!(body["reasoning"]["effort"], "high");
                assert!(body.get("output_config").is_none());
            }
            AnthropicMessages | VertexAnthropic => {
                assert_eq!(body["output_config"]["effort"], "high");
                assert_eq!(body["thinking"]["type"], "adaptive");
                assert!(body.get("reasoning").is_none());
            }
            OpenAiCompatible | WatsonxChat => {
                assert_eq!(body["reasoning_effort"], "high");
                assert!(body.get("thinking").is_none());
            }
        }
        assert!(crate::routes::request_validation::validate(&self::request(
            family,
            Some("unsupported")
        )?)
        .is_err());
        crate::routes::request_validation::validate(&self::request(family, None)?)?;
    }
    assert!(crate::routes::request_validation::validate(&request(
        AnthropicMessages,
        Some("minimal")
    )?)
    .is_err());
    Ok(())
}
