//! Cohere V1 discovery and V2 completion verification before vault publication.
use super::*;
use serde_json::{json, Value};

async fn document(request: reqwest::RequestBuilder) -> Result<Value, KeyError> {
    let mut response = request.send().await.map_err(|_| KeyError::Unreachable)?;
    if matches!(response.status().as_u16(), 401 | 403 | 498) {
        return Err(KeyError::Refused);
    }
    if !response.status().is_success() {
        return Err(KeyError::Listing);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| KeyError::Listing)? {
        if chunk.len() > (4 * 1024 * 1024usize).saturating_sub(bytes.len()) {
            return Err(KeyError::Listing);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| KeyError::Listing)
}

fn model_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

pub(super) async fn verify(base: &str, secret: VaultSecret) -> Result<VerifiedKey, KeyError> {
    tokio::time::timeout(std::time::Duration::from_secs(45), listing(base, secret))
        .await
        .map_err(|_| KeyError::Unreachable)?
}

async fn listing(base: &str, secret: VaultSecret) -> Result<VerifiedKey, KeyError> {
    let binding =
        DestinationBinding::key(ApiDestination::parse(base).map_err(|_| KeyError::Invalid)?);
    let CredentialView::ApiKey(key) = secret.expose() else {
        return Err(KeyError::Invalid);
    };
    let client = crate::chatgpt::bounded_client(base.starts_with("https://"))
        .map_err(|_| KeyError::Unreachable)?;
    let mut rows = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut tokens = std::collections::BTreeSet::new();
    let mut token: Option<String> = None;
    let mut bytes = 0usize;
    for page in 0..16 {
        let mut url = reqwest::Url::parse(&format!("{}/v1/models", base.trim_end_matches('/')))
            .map_err(|_| KeyError::Invalid)?;
        url.query_pairs_mut()
            .append_pair("endpoint", "chat")
            .append_pair("page_size", "1000");
        if let Some(token) = &token {
            url.query_pairs_mut().append_pair("page_token", token);
        }
        binding
            .check_api(url.as_str())
            .map_err(|_| KeyError::Invalid)?;
        let value = document(client.get(url).bearer_auth(key)).await?;
        bytes = bytes.saturating_add(value.to_string().len());
        if bytes > 4 * 1024 * 1024 {
            return Err(KeyError::Listing);
        }
        for model in value["models"]
            .as_array()
            .filter(|rows| rows.len() <= 1024)
            .ok_or(KeyError::Listing)?
        {
            let name = model["name"]
                .as_str()
                .filter(|name| model_id(name))
                .ok_or(KeyError::Listing)?;
            if !seen.insert(name.to_owned()) || seen.len() > 1024 {
                return Err(KeyError::Listing);
            }
            if match model.get("is_deprecated") {
                None => false,
                Some(value) => value.as_bool().ok_or(KeyError::Listing)?,
            } {
                continue;
            }
            let endpoints = model["endpoints"].as_array().ok_or(KeyError::Listing)?;
            if !endpoints.iter().any(|endpoint| endpoint == "chat") {
                continue;
            }
            let context = model["context_length"]
                .as_u64()
                .filter(|count| *count > 0)
                .ok_or(KeyError::Listing)?;
            rows.push(json!({"id":name,"max_model_len":context}));
        }
        match value.get("next_page_token") {
            None | Some(Value::Null) => {
                if rows.is_empty() {
                    return Err(KeyError::Listing);
                }
                return Ok(VerifiedKey {
                    watsonx_project: None,
                    cloudflare: None,
                    base: base.to_owned(),
                    secret,
                    binding,
                    listing: json!({"data":rows}).to_string().into_bytes(),
                });
            }
            Some(next) => {
                let next = next
                    .as_str()
                    .filter(|s| {
                        !s.is_empty() && s.len() <= 2048 && !s.chars().any(char::is_control)
                    })
                    .ok_or(KeyError::Listing)?;
                if !tokens.insert(next.to_owned()) || page == 15 {
                    return Err(KeyError::Listing);
                }
                token = Some(next.to_owned());
            }
        }
    }
    Err(KeyError::Listing)
}

/// Validate native generation with the discovered model before installing its key.
pub async fn verify_cohere_completion(
    base: &str,
    verified: &VerifiedKey,
    model: &str,
) -> Result<(), KeyError> {
    if !model_id(model) {
        return Err(KeyError::Invalid);
    }
    let url = format!("{}/v2/chat", base.trim_end_matches('/'));
    verified
        .binding
        .check_api(&url)
        .map_err(|_| KeyError::Invalid)?;
    let CredentialView::ApiKey(key) = verified.secret.expose() else {
        return Err(KeyError::Invalid);
    };
    let client = crate::chatgpt::bounded_client(base.starts_with("https://"))
        .map_err(|_| KeyError::Unreachable)?;
    let body = json!({
        "model": model,
        "messages": [{"role":"user","content":"Reply with OK."}],
        "stream": false,
        "max_tokens": 2048,
    });
    let value = document(
        client
            .post(url)
            .bearer_auth(key)
            .header("content-type", "application/json")
            .body(body.to_string()),
    )
    .await?;
    if !matches!(
        value["finish_reason"].as_str(),
        Some("COMPLETE" | "STOP_SEQUENCE")
    ) || value["message"]["role"] != "assistant"
        || !value["message"]["content"].as_array().is_some_and(|parts| {
            parts.iter().any(|part| {
                part["type"] == "text"
                    && part["text"]
                        .as_str()
                        .is_some_and(|text| !text.trim().is_empty())
            })
        })
    {
        return Err(KeyError::Completion);
    }
    Ok(())
}
