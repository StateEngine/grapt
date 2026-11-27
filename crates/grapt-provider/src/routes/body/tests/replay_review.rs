//! Ordinary provider bodies and prefix commitments across protocol families.
use super::*;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn inputs(continued: bool) -> Result<Vec<CodexInputItem>> {
    let mut input = vec![
        system("Stable instructions π").into(),
        user("Read \"a.txt\"\n中").into(),
    ];
    if continued {
        let call = ToolCallId::new("call_review").ok_or("call")?;
        input.extend([
            CodexInputItem::AssistantResponseStart,
            InputMessage {
                role: MessageRole::Assistant,
                content: "Checking π".into(),
            }
            .into(),
            CodexInputItem::ToolCall {
                call: call.clone(),
                tool: ToolName::new("read_file").ok_or("tool")?,
                arguments: CodexToolText::try_new("{\"path\":\"a.txt\"}".into())?,
            },
            CodexInputItem::ToolOutput {
                call,
                output: CodexToolText::try_new("quoted \"result\"\n中".into())?,
                is_error: false,
            },
            user("Continue after restart").into(),
        ]);
    }
    Ok(input)
}
fn requests(input: Vec<CodexInputItem>, cached: bool) -> Result<Vec<ProviderRequest>> {
    let model = model("ordinary", "model");
    let session: SessionId = "ses_review_stable".parse()?;
    let key = cached
        .then(|| PromptCacheKey::for_session(&session))
        .transpose()?;
    let anthropic = AnthropicMessagesRequest {
        model: model.clone(),
        messages: input.clone(),
        max_tokens: bound(4096),
        thinking: None,
        uncached_system_suffix: None,
        effort: Default::default(),
        instruction_mode: Default::default(),
        prompt_cache: if cached {
            crate::AnthropicPromptCache::StablePrefix
        } else {
            crate::AnthropicPromptCache::Off
        },
        tools: vec![tool()],
        web_search: None,
        tool_choice: None,
    };
    let compatible = OpenAiCompatibleRequest {
        reasoning_replay: Default::default(),
        model: model.clone(),
        messages: input.clone(),
        max_tokens: Some(bound(4096)),
        prompt_cache: if cached {
            crate::CompatiblePromptCache::AnthropicBreakpoints
        } else {
            crate::CompatiblePromptCache::Off
        },
        tools: vec![tool()],
        tool_choice: None,
        routing_session: key.clone(),
    };
    let mut watsonx = compatible.clone();
    watsonx.prompt_cache = crate::CompatiblePromptCache::Off;
    watsonx.routing_session = None;
    let gemini = crate::GoogleGeminiRequest {
        model: model.clone(),
        messages: input.clone(),
        max_tokens: Some(bound(4096)),
        tools: vec![tool()],
        tool_choice: None,
    };
    Ok(vec![
        ProviderRequest::OpenAiResponses(OpenAiResponsesRequest {
            model: model.clone(),
            input: input.clone(),
            max_output_tokens: Some(bound(4096)),
            tools: vec![tool()],
            web_search: None,
            tool_choice: None,
            prompt_cache_key: key.clone(),
        }),
        ProviderRequest::ChatGptCodex(ChatGptCodexRequest {
            model: model.clone(),
            input: input.clone(),
            tools: vec![tool()],
            web_search: None,
            tool_choice: None,
            prompt_cache_key: key,
            reasoning: Default::default(),
            turn: Default::default(),
        }),
        ProviderRequest::AnthropicMessages(anthropic.clone()),
        ProviderRequest::VertexAnthropic(anthropic),
        ProviderRequest::OpenAiCompatible(compatible.clone()),
        ProviderRequest::WatsonxChat(watsonx),
        ProviderRequest::BedrockConverse(crate::BedrockConverseRequest {
            model: model.clone(),
            messages: input.clone(),
            max_tokens: bound(4096),
            tools: vec![tool()],
            tool_choice: None,
        }),
        ProviderRequest::GoogleGemini(gemini.clone()),
        ProviderRequest::VertexGemini(gemini),
        ProviderRequest::CohereChat(crate::CohereChatRequest {
            model,
            messages: input,
            max_tokens: Some(bound(4096)),
            tools: vec![tool()],
            tool_choice: None,
        }),
    ])
}
#[test]
fn review_reasoning_ordinary_body_and_prefix_matrix() -> Result {
    for continued in [false, true] {
        for cached in [false, true] {
            let input = inputs(continued)?;
            for request in requests(input.clone(), cached)? {
                crate::routes::request_validation::validate(&request)?;
                let body = body(&request);
                assert!(body.to_string().contains("中"));
                let digest = crate::prefix_digest(&request, input.len()).ok_or("prefix digest")?;
                let mut bodies = vec![("default", body)];
                if let ProviderRequest::OpenAiCompatible(compatible) = &request {
                    bodies.push((
                        "groq",
                        super::super::compatible_body(compatible, crate::CompatibleProtocol::Groq)?,
                    ));
                    bodies.push((
                        "completion-tokens",
                        super::super::compatible_body(
                            compatible,
                            crate::CompatibleProtocol::CompletionTokens,
                        )?,
                    ));
                }
                for (format, body) in bodies {
                    eprintln!(
                        "REPLAY_REVIEW {}",
                        serde_json::json!({"family":format!("{:?}",request.family()), "format":format,"continued":continued,"cached":cached,"body":String::from_utf8(serde_json::to_vec(&body)?)?,"prefix":digest})
                    );
                }
            }
        }
    }
    Ok(())
}
#[test]
fn review_reasoning_requires_an_explicit_originating_response_boundary() -> Result {
    let mut request = requests(inputs(false)?, false)?
        .into_iter()
        .find_map(|request| match request {
            ProviderRequest::OpenAiCompatible(request) => Some(request),
            _ => None,
        })
        .ok_or("compatible request")?;
    request.reasoning_replay = crate::CompatibleReasoningReplay::ReasoningContent;
    request
        .messages
        .push(CodexInputItem::CompatibleReasoning(CodexToolText::try_new(
            "orphan reasoning".into(),
        )?));
    request.messages.push(user("Next question").into());
    assert!(
        crate::transport::fixtures::adapter(1)
            .stage(crate::transport::fixtures::input(
                "attempt_orphan_reasoning",
                "http://127.0.0.1:1",
                crate::ProtocolFamily::OpenAiCompatible,
                ProviderRequest::OpenAiCompatible(request),
            ))
            .is_err(),
        "unattributed reasoning must be rejected before transport"
    );
    Ok(())
}

#[test]
fn review_reasoning_concatenates_only_within_its_own_assistant_response() -> Result {
    let mut request = requests(inputs(false)?, false)?
        .into_iter()
        .find_map(|request| match request {
            ProviderRequest::OpenAiCompatible(request) => Some(request),
            _ => None,
        })
        .ok_or("compatible request")?;
    request.reasoning_replay = crate::CompatibleReasoningReplay::ReasoningContent;
    let reasoning = |text: &str| -> Result<CodexInputItem> {
        Ok(CodexInputItem::CompatibleReasoning(CodexToolText::try_new(
            text.into(),
        )?))
    };
    request.messages.extend([
        CodexInputItem::AssistantResponseStart,
        reasoning("first π")?,
        reasoning("second 中")?,
        InputMessage {
            role: MessageRole::Assistant,
            content: "first response".into(),
        }
        .into(),
        user("new prompt").into(),
        CodexInputItem::AssistantResponseStart,
        reasoning("separate response reasoning")?,
        InputMessage {
            role: MessageRole::Assistant,
            content: "second response".into(),
        }
        .into(),
        user("continue").into(),
    ]);
    crate::routes::request_validation::validate(&ProviderRequest::OpenAiCompatible(
        request.clone(),
    ))?;
    for format in [
        crate::CompatibleProtocol::OpenAi,
        crate::CompatibleProtocol::Groq,
        crate::CompatibleProtocol::CompletionTokens,
    ] {
        let body = super::super::compatible_body(&request, format)?;
        let messages = body["messages"].as_array().ok_or("messages")?;
        let assistant: Vec<_> = messages
            .iter()
            .filter(|message| message["role"] == "assistant")
            .collect();
        assert_eq!(assistant.len(), 2);
        assert_eq!(assistant[0]["content"], "first response");
        assert_eq!(assistant[0]["reasoning_content"], "first πsecond 中");
        assert_eq!(assistant[1]["content"], "second response");
        assert_eq!(
            assistant[1]["reasoning_content"],
            "separate response reasoning"
        );
    }
    Ok(())
}
