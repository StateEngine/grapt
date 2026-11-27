//! Gateway listings retain only the selected model provider's bundled rows.
use super::*;
use grapt_types::cloudflare::{CloudflareGatewayScope, CloudflareWire};

impl KeyProvider {
    pub fn connection_stem(self) -> String {
        match self.cloudflare {
            Some(provider) => format!("{}-{}", self.id, provider.as_str()),
            None => self.id.into(),
        }
    }
    pub fn with_cloudflare(mut self, scope: &CloudflareGatewayScope) -> Result<Self, String> {
        if self.id != "cloudflare-ai-gateway" {
            return Err("gateway coordinates apply only to Cloudflare AI Gateway".into());
        }
        self.cloudflare = Some(scope.provider());
        self.family = match scope.wire() {
            CloudflareWire::Responses => Family::OpenaiResponses,
            CloudflareWire::Messages => Family::AnthropicMessages,
            CloudflareWire::Chat => Family::OpenaiCompatible,
        };
        Ok(self)
    }

    pub fn cloudflare_listing(self) -> Result<Vec<u8>, String> {
        let provider = self
            .cloudflare
            .ok_or("choose a Cloudflare model provider")?;
        let prefix = format!("{}/", provider.as_str());
        let models = snapshot()?[self.id]["models"]
            .as_object()
            .ok_or("Cloudflare model catalog is unavailable")?;
        let rows: Vec<Value> = models
            .iter()
            .filter_map(|(id, metadata)| {
                let context = metadata["limit"]["context"].as_u64()?;
                (id.starts_with(&prefix)
                    && context > 0
                    && metadata["tool_call"].as_bool() == Some(true)
                    && metadata["modalities"]["output"]
                        .as_array()
                        .is_some_and(|items| items.iter().any(|item| item == "text")))
                .then(|| serde_json::json!({"id":id,"max_model_len":context}))
            })
            .collect();
        if rows.is_empty() {
            return Err("the selected Cloudflare provider has no supported text model".into());
        }
        let listing = serde_json::to_vec(&serde_json::json!({"data":rows}))
            .map_err(|_| "Cloudflare model list cannot be encoded")?;
        validate_listing(&listing)?;
        Ok(listing)
    }
}

pub fn metadata() -> Result<Value, String> {
    Ok(snapshot()?["cloudflare-ai-gateway"].clone())
}
