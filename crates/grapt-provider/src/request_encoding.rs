//! The same request projection for transport and conservative local sizing.
use crate::routes::body::{anthropic_body, codex_body, compatible_body, openai_responses_body};
use crate::{CompatibleProtocol, ProviderRequest};
use grapt_types::{cloudflare::CloudflareGatewayScope, WatsonxProjectId};
use std::io::{self, Write};

#[derive(Debug, Clone)]
pub struct RequestEncoding {
    compatible: CompatibleProtocol,
    cloudflare: Option<CloudflareGatewayScope>,
    watsonx_project: Option<WatsonxProjectId>,
}
impl Default for RequestEncoding {
    fn default() -> Self {
        Self::new(CompatibleProtocol::OpenAi, None, None)
    }
}
impl RequestEncoding {
    pub fn new(
        compatible: CompatibleProtocol,
        cloudflare: Option<CloudflareGatewayScope>,
        watsonx_project: Option<WatsonxProjectId>,
    ) -> Self {
        Self {
            compatible,
            cloudflare,
            watsonx_project,
        }
    }
    /// Encoded bytes plus the existing image allowance, never reported usage.
    pub fn estimate(&self, request: &ProviderRequest) -> Result<u64, String> {
        let body = self.body(request).map_err(str::to_owned)?;
        let mut count = Counter(0);
        serde_json::to_writer(&mut count, &body).map_err(|error| error.to_string())?;
        let items = match request {
            ProviderRequest::OpenAiResponses(r) => &r.input,
            ProviderRequest::ChatGptCodex(r) => &r.input,
            ProviderRequest::AnthropicMessages(r) | ProviderRequest::VertexAnthropic(r) => {
                &r.messages
            }
            ProviderRequest::OpenAiCompatible(r) | ProviderRequest::WatsonxChat(r) => &r.messages,
            ProviderRequest::BedrockConverse(r) => &r.messages,
            ProviderRequest::GoogleGemini(r) | ProviderRequest::VertexGemini(r) => &r.messages,
            ProviderRequest::CohereChat(r) => &r.messages,
        };
        items.iter().try_fold(count.0, |count, item| {
            let images = match item {
                crate::CodexInputItem::ImageUser { images, .. }
                | crate::CodexInputItem::ImageToolOutput { images, .. } => {
                    images.estimated_tokens()
                }
                _ => 0,
            };
            count
                .checked_add(images)
                .ok_or_else(|| "request image estimate overflowed".to_owned())
        })
    }
    pub(crate) fn body(
        &self,
        request: &ProviderRequest,
    ) -> Result<serde_json::Value, &'static str> {
        if let Some(scope) = self.cloudflare.as_ref() {
            return crate::transport::cloudflare::body(request, scope);
        }
        match request {
            ProviderRequest::OpenAiResponses(request) => openai_responses_body(request),
            ProviderRequest::ChatGptCodex(request) => codex_body(request),
            ProviderRequest::AnthropicMessages(request) => anthropic_body(request),
            ProviderRequest::VertexAnthropic(request) => crate::vertex_anthropic::body(request),
            ProviderRequest::OpenAiCompatible(request) => compatible_body(request, self.compatible),
            ProviderRequest::BedrockConverse(request) => crate::routes::bedrock::body(request),
            ProviderRequest::GoogleGemini(request) | ProviderRequest::VertexGemini(request) => {
                crate::gemini::body(request)
            }
            ProviderRequest::CohereChat(request) => crate::cohere::body(request),
            ProviderRequest::WatsonxChat(request) => crate::watsonx::body(
                request,
                self.watsonx_project
                    .as_ref()
                    .ok_or("Connect watsonx with an IBM project before starting a request")?,
            ),
        }
    }
}
struct Counter(u64);
impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len() as u64)
            .filter(|count| *count <= 64 * 1024 * 1024)
            .ok_or_else(|| io::Error::other("encoded request exceeds its byte bound"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
