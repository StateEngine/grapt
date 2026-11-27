//! Native Gemini discovery and generation validation before vault publication.
use super::*;
use serde_json::json;

pub(super) async fn document(
    request: reqwest::RequestBuilder,
) -> Result<serde_json::Value, KeyError> {
    let mut response = request.send().await.map_err(|_| KeyError::Unreachable)?;
    if matches!(response.status().as_u16(), 401 | 403) {
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
fn model_id(name: &str) -> Option<&str> {
    let id = name.strip_prefix("models/")?;
    (!id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')))
    .then_some(id)
}
pub(super) async fn verify(base: &str, secret: VaultSecret) -> Result<VerifiedKey, KeyError> {
    let binding =
        DestinationBinding::key(ApiDestination::parse(base).map_err(|_| KeyError::Invalid)?);
    let CredentialView::ApiKey(key) = secret.expose() else {
        return Err(KeyError::Invalid);
    };
    let client = crate::chatgpt::bounded_client(base.starts_with("https://"))
        .map_err(|_| KeyError::Unreachable)?;
    let mut rows = Vec::new();
    let mut token: Option<String> = None;
    let mut seen = std::collections::BTreeSet::new();
    let mut pages_bytes = 0usize;
    for page in 0..16 {
        let mut url = reqwest::Url::parse(&format!("{}/models", base.trim_end_matches('/')))
            .map_err(|_| KeyError::Invalid)?;
        url.query_pairs_mut().append_pair("pageSize", "1000");
        if let Some(token) = &token {
            url.query_pairs_mut().append_pair("pageToken", token);
        }
        binding
            .check_api(url.as_str())
            .map_err(|_| KeyError::Invalid)?;
        let request = client.get(url).header("x-goog-api-key", key);
        let value = document(request).await?;
        pages_bytes = pages_bytes.saturating_add(value.to_string().len());
        if pages_bytes > 4 * 1024 * 1024 {
            return Err(KeyError::Listing);
        }
        for model in value["models"]
            .as_array()
            .filter(|models| models.len() <= 1024)
            .ok_or(KeyError::Listing)?
        {
            let name = model["name"]
                .as_str()
                .and_then(model_id)
                .ok_or(KeyError::Listing)?;
            if !seen.insert(name.to_owned()) || seen.len() > 1024 {
                return Err(KeyError::Listing);
            }
            if model["supportedGenerationMethods"]
                .as_array()
                .is_some_and(|methods| methods.iter().any(|method| method == "generateContent"))
            {
                let limit = model["inputTokenLimit"]
                    .as_u64()
                    .filter(|limit| *limit > 0)
                    .ok_or(KeyError::Listing)?;
                rows.push(json!({"id":name,"max_model_len":limit}));
            }
        }
        match value.get("nextPageToken") {
            None => {
                if rows.is_empty() {
                    return Err(KeyError::Listing);
                }
                return Ok(VerifiedKey {
                    cloudflare: None,
                    base: base.to_owned(),
                    secret,
                    binding,
                    listing: json!({"data":rows}).to_string().into_bytes(),
                    watsonx_project: None,
                });
            }
            Some(next) => {
                let next = next
                    .as_str()
                    .filter(|text| {
                        !text.is_empty()
                            && text.len() <= 2048
                            && !text.chars().any(char::is_control)
                    })
                    .ok_or(KeyError::Listing)?;
                if token.as_deref() == Some(next) || page == 15 {
                    return Err(KeyError::Listing);
                }
                token = Some(next.to_owned());
            }
        }
    }
    Err(KeyError::Listing)
}

/// Prove a native generation completes with this key before publishing a connection.
pub async fn verify_completion(
    base: &str,
    verified: &VerifiedKey,
    model: &str,
) -> Result<(), KeyError> {
    if model_id(&format!("models/{model}")) != Some(model) {
        return Err(KeyError::Invalid);
    }
    let url = format!(
        "{}/models/{model}:generateContent",
        base.trim_end_matches('/')
    );
    verified
        .binding
        .check_api(&url)
        .map_err(|_| KeyError::Invalid)?;
    let client = crate::chatgpt::bounded_client(base.starts_with("https://"))
        .map_err(|_| KeyError::Unreachable)?;
    let request = match verified.secret.expose() {
        CredentialView::ApiKey(key) => client.post(&url).header("x-goog-api-key", key),
        CredentialView::VertexServiceAccount { access, .. } => {
            client.post(&url).bearer_auth(access)
        }
        CredentialView::VertexAdc => crate::vertex::adc::Session::open()
            .map_err(|_| KeyError::Refused)?
            .authorize_builder(client.post(&url))
            .await
            .map_err(|_| KeyError::Refused)?,
        _ => return Err(KeyError::Invalid),
    };
    let value = document(request.header("content-type", "application/json").body(json!({"contents":[{"role":"user","parts":[{"text":"Reply with OK."}]}],"generationConfig":{"maxOutputTokens":2048}}).to_string())).await?;
    let candidates = value["candidates"]
        .as_array()
        .filter(|items| items.len() == 1)
        .ok_or(KeyError::Completion)?;
    let candidate = &candidates[0];
    if candidate["finishReason"] != "STOP"
        || !candidate["content"]["parts"]
            .as_array()
            .is_some_and(|parts| {
                parts.iter().any(|part| {
                    part["thought"] != true
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
