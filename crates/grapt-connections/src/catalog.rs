//! Provider metadata shared by every connection surface.
//! Bundled model catalog seeding.
// Copyright 2025 OpenAI. See licenses/openai-codex/.
// Public metadata: models.dev, MIT; see licenses/models-dev/.
mod compatible;
#[cfg(test)]
mod listing_tests;
pub use compatible::requires_reasoning_content_replay;
pub use grapt_connections_catalog::{endpoint_provider, EndpointProvider};

use crate::store::Family;
use serde_json::Value;
use std::sync::OnceLock;
pub mod cloudflare;

pub const BUNDLED: &[u8] = include_bytes!("catalog/models.dev-2026-11-03.json");
static SNAPSHOT: OnceLock<Result<Value, String>> = OnceLock::new();

fn snapshot() -> Result<&'static Value, String> {
    let parsed = SNAPSHOT.get_or_init(|| {
        if BUNDLED.len() > 16 * 1024 * 1024 {
            return Err("bundled model catalog exceeds its byte bound".into());
        }
        let value: Value =
            serde_json::from_slice(BUNDLED).map_err(|_| "bundled model catalog is invalid")?;
        let providers = value
            .as_object()
            .ok_or("bundled provider catalog is invalid")?;
        if providers.len() > 256
            || providers
                .values()
                .map(|provider| {
                    provider["models"]
                        .as_object()
                        .map_or(0, |models| models.len())
                })
                .sum::<usize>()
                > 16384
        {
            return Err("bundled model catalog exceeds its row bound".into());
        }
        Ok(value)
    });
    parsed.as_ref().map_err(Clone::clone)
}

#[derive(Debug)]
pub struct CatalogIntegration {
    pub id: &'static str,
    pub name: &'static str,
    pub key: Option<KeyProvider>,
}

pub fn integrations() -> Result<Vec<CatalogIntegration>, String> {
    let providers = snapshot()?
        .as_object()
        .ok_or("bundled provider catalog is invalid")?;
    let mut rows: Vec<_> = providers
        .iter()
        .map(|(id, row)| {
            if row["id"].as_str() != Some(id.as_str()) || id.is_empty() || id.len() > 64 {
                return Err("bundled provider identity is invalid".into());
            }
            let name = row["name"]
                .as_str()
                .ok_or("bundled provider name is missing")?;
            Ok(CatalogIntegration {
                id,
                name,
                key: KeyProvider::from_row(id, row),
            })
        })
        .collect::<Result<_, String>>()?;
    for row in grapt_connections_catalog::local_servers() {
        if providers.contains_key(row.id) {
            continue;
        }
        let local = KeyProvider::local(row).ok_or("local server preset is invalid")?;
        rows.push(CatalogIntegration {
            id: local.id,
            name: local.name,
            key: Some(local),
        });
    }
    let custom = KeyProvider::custom();
    rows.push(CatalogIntegration {
        id: custom.id,
        name: custom.name,
        key: Some(custom),
    });
    Ok(rows)
}

/// A provider whose default SDK path has an implemented request and key shape.
#[derive(Debug, Clone, Copy)]
pub struct KeyProvider {
    id: &'static str,
    name: &'static str,
    base: &'static str,
    sdk: &'static str,
    family: Family,
    cloudflare: Option<grapt_types::cloudflare::CloudflareProviderId>,
}

impl KeyProvider {
    fn custom() -> Self {
        Self {
            id: "custom",
            cloudflare: None,
            name: "Custom / local provider",
            base: "",
            sdk: "@ai-sdk/openai-compatible",
            family: Family::OpenaiCompatible,
        }
    }

    /// A local model server the bundled catalogue does not name: the same
    /// compatible chat wire as a custom endpoint, under the server's own name.
    fn local(row: &'static EndpointProvider) -> Option<Self> {
        Some(Self {
            id: row.id,
            name: row.local?.name,
            ..Self::custom()
        })
    }

    pub fn from_id(id: &str) -> Result<Option<Self>, String> {
        if id == "custom" {
            return Ok(Some(Self::custom()));
        }
        let Some((id, row)) = snapshot()?
            .as_object()
            .and_then(|providers| providers.get_key_value(id))
        else {
            return Ok(endpoint_provider(id).and_then(Self::local));
        };
        Ok(Self::from_row(id, row))
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn from_row(id: &'static str, row: &'static Value) -> Option<Self> {
        if id == "amazon-bedrock" {
            return Some(Self {
                id,
                name: "Amazon Bedrock",
                base: "https://bedrock-runtime.us-east-1.amazonaws.com",
                sdk: "@ai-sdk/amazon-bedrock",
                cloudflare: None,
                family: Family::BedrockConverse,
            });
        }
        if matches!(id, "azure" | "azure-cognitive-services") {
            return Some(Self {
                id,
                name: row["name"].as_str()?,
                base: "",
                sdk: "@ai-sdk/azure",
                cloudflare: None,
                family: Family::OpenaiResponses,
            });
        }
        if id == "cloudflare-ai-gateway" {
            return Some(Self {
                id,
                name: row["name"].as_str()?,
                base: "https://gateway.ai.cloudflare.com",
                sdk: "ai-gateway-provider",
                family: Family::OpenaiResponses,
                cloudflare: None,
            });
        }
        // These IDs need a token exchange or cloud signing even when the
        // catalog describes their final wire format as OpenAI-compatible.
        if matches!(
            id,
            "github-copilot"
                | "github-copilot-enterprise"
                | "amazon-bedrock"
                | "amazon-bedrock-mantle"
        ) {
            return None;
        }
        if id == "watsonx" {
            return Some(Self {
                id,
                name: row["name"].as_str()?,
                base: "",
                cloudflare: None,
                sdk: "watsonx-ai-provider",
                family: Family::WatsonxChat,
            });
        }
        if matches!(id, "google-vertex" | "google-vertex-anthropic") {
            return Some(Self {
                id,
                name: row["name"].as_str()?,
                base: "",
                cloudflare: None,
                sdk: row["npm"].as_str()?,
                family: if id == "google-vertex-anthropic" {
                    Family::VertexAnthropic
                } else {
                    Family::VertexGemini
                },
            });
        }
        let sdk = row["npm"].as_str()?;
        let compatible_base = match (id, sdk) {
            ("mistral", "@ai-sdk/mistral") => Some("https://api.mistral.ai/v1"),
            ("xai", "@ai-sdk/xai") => Some("https://api.x.ai/v1"),
            ("cerebras", "@ai-sdk/cerebras") => Some("https://api.cerebras.ai/v1"),
            ("groq", "@ai-sdk/groq") => Some("https://api.groq.com/openai/v1"),
            ("deepinfra", "@ai-sdk/deepinfra") => Some("https://api.deepinfra.com/v1/openai"),
            ("togetherai", "@ai-sdk/togetherai") => Some("https://api.together.xyz/v1"),
            ("merge-gateway", "merge-gateway-ai-sdk-provider") => {
                Some("https://api-gateway.merge.dev/v1/ai-sdk")
            }
            ("salad-cloud", "@saladtechnologies-oss/ai-sdk-provider") => {
                Some("https://ai.salad.cloud/v1")
            }
            _ => None,
        };
        let family = match sdk {
            "@ai-sdk/cohere" if id == "cohere" => Family::CohereChat,
            "@ai-sdk/openai" => Family::OpenaiResponses,
            "@ai-sdk/google" if id == "google" => Family::GoogleGemini,
            "@ai-sdk/anthropic" => Family::AnthropicMessages,
            "@ai-sdk/openai-compatible" | "@openrouter/ai-sdk-provider" => Family::OpenaiCompatible,
            _ if compatible_base.is_some() => Family::OpenaiCompatible,
            _ => return None,
        };
        if row["env"].as_array()?.is_empty() {
            return None;
        }
        let explicit_workspace = (id == "databricks" || endpoint_provider(id).is_some())
            && sdk == "@ai-sdk/openai-compatible";
        let base = match id {
            "cohere" => "https://api.cohere.com",
            _ if explicit_workspace => "",
            "openai" => "https://api.openai.com/v1",
            "google" => "https://generativelanguage.googleapis.com/v1beta",
            "anthropic" => "https://api.anthropic.com/v1",
            _ => compatible_base.or_else(|| row["api"].as_str())?,
        };
        if !explicit_workspace
            && (base.contains(['{', '}', '$'])
                || grapt_login::vault::ApiDestination::parse(base).is_err())
        {
            return None;
        }
        Some(Self {
            id,
            name: row["name"].as_str()?,
            base,
            sdk,
            family,
            cloudflare: None,
        })
    }

    pub fn id(self) -> &'static str {
        self.id
    }
    pub fn name(self) -> &'static str {
        self.name
    }
    pub fn environment_names(self) -> Result<Vec<&'static str>, String> {
        // Workspace coordinates come from typed setup, never an exported secret.
        if matches!(self.id, "custom" | "databricks")
            || endpoint_provider(self.id).is_some()
            || self.id == "cloudflare-ai-gateway"
            || self.is_azure()
            || self.family == Family::BedrockConverse
            || self.family.is_vertex()
            || self.family == Family::WatsonxChat
        {
            return Ok(Vec::new());
        }
        let names = snapshot()?[self.id]["env"]
            .as_array()
            .filter(|names| names.len() <= 16)
            .ok_or("provider environment names exceed their bound")?;
        names
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|name| {
                        !name.is_empty()
                            && name.len() <= 128
                            && name
                                .bytes()
                                .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
                    })
                    .ok_or_else(|| "provider environment name is invalid".into())
            })
            .collect()
    }
    pub fn allows_environment(self, name: &str) -> Result<bool, String> {
        Ok(self.environment_names()?.contains(&name))
    }
    pub fn api_base(self) -> &'static str {
        self.base
    }
    pub fn family(self) -> Family {
        self.family
    }

    pub fn permits_empty_local_key(self) -> bool {
        self.id == "custom"
            || endpoint_provider(self.id).is_some_and(|row| row.permits_empty_local_key)
    }

    pub fn for_model(mut self, model: &str) -> Self {
        if self.messages_model(model) {
            self.family = Family::AnthropicMessages;
        }
        self
    }

    /// Whether a mixed endpoint serves `model` on its Messages wire.
    fn messages_model(self, model: &str) -> bool {
        endpoint_provider(self.id)
            .and_then(|row| row.messages_model_prefix)
            .is_some_and(|prefix| model.starts_with(prefix))
    }

    pub fn placement(self) -> grapt_login::key::KeyPlacement {
        if self.family == Family::GoogleGemini {
            grapt_login::key::KeyPlacement::Google
        } else if self.family == Family::CohereChat {
            grapt_login::key::KeyPlacement::Cohere
        } else if self.family == Family::AnthropicMessages && endpoint_provider(self.id).is_none() {
            grapt_login::key::KeyPlacement::Anthropic
        } else {
            grapt_login::key::KeyPlacement::Bearer
        }
    }
    pub fn route(self) -> &'static str {
        match self.family {
            Family::CohereChat => "cohere-chat",
            Family::BedrockConverse => "converse-stream",
            Family::AnthropicMessages | Family::VertexAnthropic => "messages",
            Family::GoogleGemini | Family::VertexGemini => "generate-content",
            Family::OpenaiResponses => "responses",
            Family::OpenaiCompatible | Family::WatsonxChat => "chat-completions",
            Family::ChatgptCodex => "responses",
        }
    }

    pub fn is_azure(self) -> bool {
        matches!(self.id, "azure" | "azure-cognitive-services")
    }

    /// An Azure deployment is an account-local alias, not a catalog model ID.
    /// Admit only the selected native Responses model; mixed Foundry routes
    /// need their own setup rather than inheriting this connection's wire.
    pub fn azure_listing(self, deployment: &str, model: &str) -> Result<Vec<u8>, String> {
        if !self.is_azure()
            || deployment.is_empty()
            || deployment.len() > 256
            || deployment.chars().any(char::is_control)
        {
            return Err("invalid Azure deployment name".into());
        }
        let metadata = &snapshot()?[self.id]["models"][model];
        if model.is_empty()
            || model.len() > 256
            || metadata["id"].as_str() != Some(model)
            || metadata["provider"]["npm"]
                .as_str()
                .is_some_and(|sdk| sdk != self.sdk)
            || metadata["provider"]["api"].is_string()
            || metadata["provider"]["shape"]
                .as_str()
                .is_some_and(|shape| shape != "responses")
            || metadata["tool_call"].as_bool() != Some(true)
            || !metadata["modalities"]["output"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item.as_str() == Some("text")))
            || metadata["limit"]["context"]
                .as_u64()
                .is_none_or(|value| value == 0)
            || metadata["limit"]["output"]
                .as_u64()
                .is_none_or(|value| value == 0)
        {
            return Err("select an Azure Responses model with known limits and tools".into());
        }
        let mut row = serde_json::json!({
            "id": deployment,
            "catalog_model_id": model,
            "max_model_len": metadata["limit"]["context"],
        });
        if let Some(options) = metadata["reasoning_options"].as_array() {
            let efforts: Vec<Value> = options
                .iter()
                .filter(|option| option["type"] == "effort")
                .filter_map(|option| option["values"].as_array())
                .flatten()
                .map(|effort| serde_json::json!({"effort":effort}))
                .collect();
            row["supported_reasoning_levels"] = efforts.into();
        }
        serde_json::to_vec(&serde_json::json!({"data":[row]}))
            .map_err(|_| "Azure deployment listing cannot be encoded".into())
    }

    // A mixed gateway's model overrides may name a different protocol/base.
    // Keep those outside this connection's issuable listing until its adapter
    // is composed; one connection must not dispatch them over the wrong wire.
    pub fn supported_listing(self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        validate_listing(bytes)?;
        if self.id == "cloudflare-ai-gateway" {
            return self.cloudflare_listing();
        }
        // Explicit endpoints check their selected model on this route;
        // catalog SDK defaults cannot override the verified coordinates. A
        // mixed endpoint serves each model on one wire, so a connection
        // publishes only the models on the wire it checked.
        if let Some(endpoint) = endpoint_provider(self.id) {
            if endpoint.messages_model_prefix.is_none() {
                return Ok(bytes.to_vec());
            }
            let messages = self.family == Family::AnthropicMessages;
            let mut listing: Value =
                serde_json::from_slice(bytes).map_err(|_| "provider model list is invalid")?;
            listing["data"]
                .as_array_mut()
                .ok_or("provider model list is invalid")?
                .retain(|row| {
                    row["id"]
                        .as_str()
                        .is_some_and(|id| self.messages_model(id) == messages)
                });
            return serde_json::to_vec(&listing)
                .map_err(|_| "provider model list cannot be encoded".into());
        }
        let mut listing: Value =
            serde_json::from_slice(bytes).map_err(|_| "provider model list is invalid")?;
        let models = &snapshot()?[self.id]["models"];
        listing["data"]
            .as_array_mut()
            .ok_or("provider model list is invalid")?
            .retain(|row| {
                let identity = if self.is_azure() {
                    "catalog_model_id"
                } else {
                    "id"
                };
                let Some(id) = row[identity].as_str() else {
                    return false;
                };
                let metadata = &models[id];
                if self.is_azure() && metadata["id"].as_str() != Some(id) {
                    return false;
                }
                if matches!(
                    self.family,
                    Family::GoogleGemini | Family::CohereChat | Family::VertexGemini
                ) && (metadata["tool_call"].as_bool() == Some(false)
                    || metadata["modalities"]["output"]
                        .as_array()
                        .is_some_and(|items| {
                            !items.iter().any(|item| item.as_str() == Some("text"))
                        }))
                {
                    return false;
                }
                metadata["provider"]["npm"]
                    .as_str()
                    .is_none_or(|sdk| sdk == self.sdk)
                    && metadata["provider"]["api"].as_str().is_none_or(|base| {
                        base.trim_end_matches('/') == self.base.trim_end_matches('/')
                            || (self.family == Family::CohereChat
                                && base.trim_end_matches('/') == format!("{}/v2", self.base))
                    })
            });
        serde_json::to_vec(&listing).map_err(|_| "provider model list cannot be encoded".into())
    }

    pub fn bedrock_listing(self, model: &str) -> Result<Vec<u8>, String> {
        if self.family != Family::BedrockConverse
            || model.is_empty()
            || model.len() > 256
            || model.chars().any(char::is_control)
        {
            return Err("invalid Bedrock model identity".into());
        }
        let metadata = &snapshot()?[self.id]["models"][model];
        if metadata["provider"]["npm"]
            .as_str()
            .is_some_and(|sdk| sdk != self.sdk)
            || metadata["provider"]["api"].as_str().is_some()
            || metadata["tool_call"].as_bool() == Some(false)
            || metadata["limit"]["context"].as_u64().is_none_or(|n| n == 0)
            || metadata["limit"]["output"].as_u64().is_none_or(|n| n == 0)
        {
            return Err("select a Bedrock model with known context and output limits".into());
        }
        serde_json::to_vec(&serde_json::json!({"data":[{"id":model}]}))
            .map_err(|_| "Bedrock model listing cannot be encoded".into())
    }

    /// Admit one known native model for this Vertex publisher.
    pub fn vertex_listing(self, model: &str) -> Result<Vec<u8>, String> {
        if !self.family.is_vertex()
            || model.is_empty()
            || model.len() > 256
            || !model.bytes().all(|b| {
                b.is_ascii_alphanumeric()
                    || b"-_.".contains(&b)
                    || (self.family == Family::VertexAnthropic && b == b'@')
            })
        {
            return Err("choose a known native Vertex model".into());
        }
        let metadata = &snapshot()?[self.id]["models"][model];
        let context = metadata["limit"]["context"]
            .as_u64()
            .filter(|v| *v > 0)
            .ok_or("Vertex model has no known context limit")?;
        if metadata["limit"]["output"].as_u64().is_none_or(|v| v == 0)
            || metadata["tool_call"] != true
            || !metadata["modalities"]["output"]
                .as_array()
                .is_some_and(|items| items.iter().any(|v| v == "text"))
        {
            return Err("choose a Vertex model that supports text and tools".into());
        }
        let listing =
            serde_json::to_vec(&serde_json::json!({"data":[{"id":model,"max_model_len":context}]}))
                .map_err(|_| "Vertex model cannot be encoded")?;
        let supported = self.supported_listing(&listing)?;
        if self.default_model(&supported)? != model {
            return Err("Vertex model uses an unsupported native protocol".into());
        }
        Ok(supported)
    }

    pub fn watsonx_listing(self, model: &str) -> Result<Vec<u8>, String> {
        if self.family != Family::WatsonxChat {
            return Err("choose a native watsonx connection".into());
        }
        let metadata = &snapshot()?[self.id]["models"][model];
        let context = metadata["limit"]["context"]
            .as_u64()
            .filter(|v| *v > 0)
            .ok_or("choose a known watsonx model with a context limit")?;
        if metadata["limit"]["output"].as_u64().is_none_or(|v| v == 0)
            || metadata["tool_call"] != true
            || !metadata["modalities"]["output"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|v| v == "text"))
        {
            return Err("choose a watsonx model that supports text and tools".into());
        }
        let mut row = serde_json::json!({"id":model,"max_model_len":context});
        if let Some(options) = metadata["reasoning_options"].as_array() {
            let mut levels = Vec::new();
            for option in options.iter().filter(|option| option["type"] == "effort") {
                for value in option["values"]
                    .as_array()
                    .ok_or("watsonx effort options are invalid")?
                {
                    let effort = value
                        .as_str()
                        .filter(|v| matches!(*v, "low" | "medium" | "high"))
                        .ok_or("watsonx effort option is unsupported")?;
                    levels.push(serde_json::json!({"effort":effort}));
                }
            }
            if !levels.is_empty() {
                row["supported_reasoning_levels"] = levels.into();
            }
        }
        let bytes = serde_json::to_vec(&serde_json::json!({"data":[row]}))
            .map_err(|_| "watsonx model cannot be encoded")?;
        let supported = self.supported_listing(&bytes)?;
        if self.default_model(&supported)? != model {
            return Err("watsonx model uses an unsupported native protocol".into());
        }
        Ok(supported)
    }

    pub fn default_model(self, listing: &[u8]) -> Result<String, String> {
        validate_listing(listing)?;
        let listing: Value =
            serde_json::from_slice(listing).map_err(|_| "provider model list is invalid")?;
        let rows = listing["data"]
            .as_array()
            .ok_or("provider model list is invalid")?;
        let models = &snapshot()?[self.id]["models"];
        if self.id == "openai" {
            // Old Chat Completions-only models can still appear in /models.
            // Prefer an inexpensive model that supports the Responses route.
            for preferred in [
                "gpt-4.1-nano",
                "gpt-4.1-mini",
                "gpt-4.1",
                "gpt-4o-mini",
                "gpt-4o",
            ] {
                if rows.iter().any(|row| row["id"].as_str() == Some(preferred)) {
                    return Ok(preferred.into());
                }
            }
        }
        rows.iter()
            .find(|row| runnable(models, row))
            .and_then(|row| row["id"].as_str())
            .map(str::to_owned)
            .ok_or_else(|| "provider lists no supported model with a known context limit".into())
    }

    /// Whether `model` is listed with what a run needs: a known context
    /// limit, text output and tool calls.
    pub fn runs(self, listing: &[u8], model: &str) -> Result<bool, String> {
        validate_listing(listing)?;
        let listing: Value =
            serde_json::from_slice(listing).map_err(|_| "provider model list is invalid")?;
        let models = &snapshot()?[self.id]["models"];
        Ok(listing["data"].as_array().is_some_and(|rows| {
            rows.iter()
                .any(|row| row["id"].as_str() == Some(model) && runnable(models, row))
        }))
    }

    /// Whether a listed row can run a session before its window is known:
    /// text output and tool calls, as `runs` reads them.
    pub fn capable(self, row: &Value) -> Result<bool, String> {
        Ok(capable(&snapshot()?[self.id]["models"], row))
    }
}

fn runnable(models: &Value, row: &Value) -> bool {
    let Some(id) = row["id"].as_str() else {
        return false;
    };
    let context = models[id]["limit"]["context"]
        .as_u64()
        .or_else(|| row["max_model_len"].as_u64());
    context.is_some_and(|context| context > 0) && capable(models, row)
}

fn capable(models: &Value, row: &Value) -> bool {
    let Some(id) = row["id"].as_str() else {
        return false;
    };
    let model = &models[id];
    // What the live row states about its request surface and output wins
    // over the bundled catalogue, as the model inventory reads it.
    let output = row["output_modalities"]
        .as_array()
        .filter(|items| !items.is_empty())
        .or_else(|| model["modalities"]["output"].as_array());
    let text = output.is_none_or(|items| items.iter().any(|item| item.as_str() == Some("text")));
    let tools = match row["supported_parameters"].as_array() {
        Some(names) => names.iter().any(|name| name.as_str() == Some("tools")),
        None => model["tool_call"].as_bool() != Some(false),
    };
    text && tools
}

fn validate_listing(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("provider model list exceeds its byte bound".into());
    }
    let listing: Value =
        serde_json::from_slice(bytes).map_err(|_| "provider model list is invalid")?;
    let rows = listing["data"]
        .as_array()
        .ok_or("provider model list is invalid")?;
    if rows.len() > 1024
        || rows.iter().any(|row| {
            row["id"]
                .as_str()
                .is_none_or(|id| id.is_empty() || id.len() > 256)
        })
    {
        return Err("provider model list exceeds its row or identity bound".into());
    }
    Ok(())
}
