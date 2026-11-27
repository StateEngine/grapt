//! A gateway token is installable only after completed native generation.
use super::*;
use grapt_types::{
    cloudflare::{CloudflareGatewayScope, CloudflareWire},
    ModelId,
};

pub fn check_base(scope: &CloudflareGatewayScope, base: &str) -> Result<(), KeyError> {
    if base == scope.api_base() {
        return Ok(());
    }
    #[cfg(feature = "loopback-testkit")]
    {
        let url = reqwest::Url::parse(base).map_err(|_| KeyError::Invalid)?;
        if url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.port().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == scope.api_path()
        {
            return Ok(());
        }
    }
    Err(KeyError::Invalid)
}

#[cfg(feature = "loopback-testkit")]
pub fn fixture_base(scope: &CloudflareGatewayScope, origin: &str) -> Result<String, KeyError> {
    let url = reqwest::Url::parse(origin).map_err(|_| KeyError::Invalid)?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(KeyError::Invalid);
    }
    let base = format!("{}{}", url.origin().ascii_serialization(), scope.api_path());
    check_base(scope, &base)?;
    Ok(base)
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub async fn verify(
    base: &str,
    secret: VaultSecret,
    scope: CloudflareGatewayScope,
    model: ModelId,
    listing: Vec<u8>,
) -> Result<VerifiedKey, KeyError> {
    check_base(&scope, base)?;
    if listing.is_empty() || listing.len() > 4 * 1024 * 1024 {
        return Err(KeyError::Invalid);
    }
    let model = scope.wire_model(&model).map_err(|_| KeyError::Invalid)?;
    let binding =
        DestinationBinding::key(ApiDestination::parse(base).map_err(|_| KeyError::Invalid)?);
    let url = format!("{base}{}", scope.request_path());
    binding.check_api(&url).map_err(|_| KeyError::Invalid)?;
    let client = crate::chatgpt::bounded_client(base.starts_with("https://"))
        .map_err(|_| KeyError::Unreachable)?;
    let CredentialView::ApiKey(key) = secret.expose() else {
        return Err(KeyError::Invalid);
    };
    let mut gateway_auth = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))
        .map_err(|_| KeyError::Invalid)?;
    gateway_auth.set_sensitive(true);
    let (body, request) = match scope.wire() {
        CloudflareWire::Responses => (
            serde_json::json!({"model":model.as_str(),"input":[{"role":"user","content":"Reply with OK."}],"max_output_tokens":2048,"store":false}),
            client
                .post(&url)
                .header("cf-aig-authorization", gateway_auth.clone()),
        ),
        CloudflareWire::Messages => (
            serde_json::json!({"model":model.as_str(),"messages":[{"role":"user","content":"Reply with OK."}],"max_tokens":2048}),
            client
                .post(&url)
                .header("cf-aig-authorization", gateway_auth)
                .header("anthropic-version", "2023-06-01"),
        ),
        CloudflareWire::Chat => (
            serde_json::json!({"model":model.as_str(),"messages":[{"role":"user","content":"Reply with OK."}],"max_tokens":2048}),
            client
                .post(&url)
                .bearer_auth(key)
                .header("cf-aig-gateway-id", scope.gateway().as_str()),
        ),
    };
    let mut response = request
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(|_| KeyError::Unreachable)?;
    if response.status().is_server_error() || response.status().as_u16() == 429 {
        return Err(KeyError::Unreachable);
    }
    if matches!(response.status().as_u16(), 401 | 403) {
        return Err(KeyError::Refused);
    }
    if !response.status().is_success() {
        return Err(KeyError::Completion);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| KeyError::Unreachable)? {
        if chunk.len() > (64 * 1024usize).saturating_sub(bytes.len()) {
            return Err(KeyError::Completion);
        }
        bytes.extend_from_slice(&chunk);
    }
    let reply: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| KeyError::Completion)?;
    if !reply.is_object() || reply.get("error").is_some_and(|error| !error.is_null()) {
        return Err(KeyError::Completion);
    }
    let nonempty =
        |text: &serde_json::Value| text.as_str().is_some_and(|text| !text.trim().is_empty());
    let completed = match scope.wire() {
        CloudflareWire::Responses => {
            reply["status"] == "completed"
                && reply["output"].as_array().is_some_and(|items| {
                    items.iter().any(|item| {
                        item["type"] == "message"
                            && item["role"] == "assistant"
                            && item["content"].as_array().is_some_and(|content| {
                                content.iter().any(|part| {
                                    part["type"] == "output_text" && nonempty(&part["text"])
                                })
                            })
                    })
                })
        }
        CloudflareWire::Messages => {
            reply["type"] == "message"
                && reply["role"] == "assistant"
                && reply["stop_reason"] == "end_turn"
                && reply["content"].as_array().is_some_and(|items| {
                    items
                        .iter()
                        .any(|item| item["type"] == "text" && nonempty(&item["text"]))
                })
        }
        CloudflareWire::Chat => reply["choices"].as_array().is_some_and(|choices| {
            choices.len() == 1
                && choices[0]["finish_reason"] == "stop"
                && choices[0]["message"]["role"] == "assistant"
                && nonempty(&choices[0]["message"]["content"])
        }),
    };
    if !completed {
        return Err(KeyError::Completion);
    }
    Ok(VerifiedKey {
        watsonx_project: None,
        base: base.to_owned(),
        secret,
        binding,
        listing,
        cloudflare: Some(scope),
    })
}
