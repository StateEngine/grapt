use super::*;

/// A SHA-256 over `request`'s protocol, model, tools and tool choice and every
/// byte of its first `items` input items, opaque replay included, so a later
/// request can be checked to repeat this one exactly without retaining it.
/// `None` when the request has fewer items. Not an identity; never displayed.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub fn prefix_digest(request: &ProviderRequest, items: usize) -> Option<[u8; 32]> {
    use sha2::Digest;
    let input = match request {
        ProviderRequest::OpenAiResponses(request) => &request.input,
        ProviderRequest::ChatGptCodex(request) => &request.input,
        ProviderRequest::AnthropicMessages(request) | ProviderRequest::VertexAnthropic(request) => {
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
    let items = input.get(..items)?;
    let mut hash = sha2::Sha256::new();
    let mut field = |bytes: &[u8]| {
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    };
    field(format!("{:?}", request.family()).as_bytes());
    field(format!("{:?}", request.model()).as_bytes());
    field(format!("{:?}{:?}", request.tools(), request.tool_choice()).as_bytes());
    let search = match request {
        ProviderRequest::OpenAiResponses(request) => request.web_search.as_ref(),
        ProviderRequest::ChatGptCodex(request) => request.web_search.as_ref(),
        ProviderRequest::AnthropicMessages(_)
        | ProviderRequest::VertexAnthropic(_)
        | ProviderRequest::OpenAiCompatible(_)
        | ProviderRequest::BedrockConverse(_)
        | ProviderRequest::GoogleGemini(_)
        | ProviderRequest::CohereChat(_)
        | ProviderRequest::WatsonxChat(_)
        | ProviderRequest::VertexGemini(_) => None,
    };
    if let Some(search) = search {
        field(b"native-web-search");
        field(search.wire().to_string().as_bytes());
    }
    if let Some(search) = request.anthropic_web_search() {
        field(b"anthropic-web-search");
        field(search.wire().to_string().as_bytes());
    }

    if let ProviderRequest::AnthropicMessages(request) = request {
        if let Some(suffix) = request
            .uncached_system_suffix
            .as_deref()
            .filter(|text| !text.is_empty())
        {
            field(b"uncached-system-suffix");
            field(suffix.as_bytes());
        }
    }

    for item in items {
        match item {
            CodexInputItem::EffortUpdate(effort) => {
                field(b"effort");
                field(effort.as_str().as_bytes());
            }
            CodexInputItem::CompatibleReasoning(text) => {
                field(b"compatible-reasoning");
                field(text.as_str().as_bytes());
            }
            CodexInputItem::ProviderReplay(replay) => {
                field(b"replay");
                replay.digest_fields(&mut field);
            }
            CodexInputItem::Message(message) => {
                field(match message.role {
                    MessageRole::System => b"system",
                    MessageRole::User => b"user",
                    MessageRole::Assistant => b"assistant",
                });
                field(message.content.as_bytes());
            }
            CodexInputItem::ImageUser { text, images } => {
                field(b"image_user");
                field(text.as_str().as_bytes());
                images.digest_fields(&mut field);
            }
            CodexInputItem::ImageToolOutput {
                call,
                output,
                images,
            } => {
                field(b"image_tool_output");
                field(call.as_str().as_bytes());
                field(output.as_str().as_bytes());
                images.digest_fields(&mut field);
            }
            CodexInputItem::ToolCall {
                call,
                tool,
                arguments,
            } => {
                field(b"call");
                field(call.as_str().as_bytes());
                field(tool.as_str().as_bytes());
                // The digest covers the arguments as stored. Arguments that are
                // not a JSON object go on the wire as a marker object; that
                // projection is deterministic, so the same history still
                // repeats exactly.
                field(arguments.as_str().as_bytes());
            }
            CodexInputItem::TextToolCall { call, tool, input } => {
                field(b"text-call");
                field(call.as_str().as_bytes());
                field(tool.as_str().as_bytes());
                field(input.as_str().as_bytes());
            }
            CodexInputItem::ToolOutput {
                call,
                output,
                is_error,
            } => {
                field(if *is_error { b"error" } else { b"output" });
                field(call.as_str().as_bytes());
                field(output.as_str().as_bytes());
            }
            CodexInputItem::AssistantResponseStart => field(b"assistant_response_start"),
            CodexInputItem::CompactionTrigger => field(b"compaction_trigger"),
        }
    }
    Some(hash.finalize().into())
}
