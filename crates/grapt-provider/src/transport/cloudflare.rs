//! Project a scoped gateway model without changing its issued identity.
use crate::routes::body::{
    anthropic_body_with_model, compatible_body, openai_responses_body_with_model,
};
use crate::ProviderRequest;
use grapt_types::cloudflare::CloudflareGatewayScope;

pub(crate) fn body(
    request: &ProviderRequest,
    scope: &CloudflareGatewayScope,
) -> Result<serde_json::Value, &'static str> {
    if crate::credential_placement(
        request.family(),
        Some(scope),
        crate::MessagesAuthMode::ApiKey,
    )
    .is_none()
    {
        return Err("Cloudflare scope does not match the request protocol");
    }
    let wire_model = scope
        .wire_model(request.model().model())
        .map_err(|_| "the model does not belong to the selected Cloudflare provider")?;
    match request {
        ProviderRequest::OpenAiResponses(request) => {
            openai_responses_body_with_model(request, &wire_model)
        }
        ProviderRequest::AnthropicMessages(request) => {
            anthropic_body_with_model(request, &wire_model)
        }
        ProviderRequest::OpenAiCompatible(request) => {
            compatible_body(request, crate::CompatibleProtocol::OpenAi)
        }
        ProviderRequest::ChatGptCodex(_)
        | ProviderRequest::BedrockConverse(_)
        | ProviderRequest::GoogleGemini(_)
        | ProviderRequest::CohereChat(_)
        | ProviderRequest::VertexGemini(_)
        | ProviderRequest::VertexAnthropic(_)
        | ProviderRequest::WatsonxChat(_) => Err("Cloudflare does not support this protocol"),
    }
}
