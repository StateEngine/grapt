//! Verify the selected native Vertex publisher before publishing its vault.
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexProtocol {
    Gemini,
    Anthropic,
}

/// Verify a service-account exchange and native completion before publication.
pub async fn verify_vertex(
    base: &str,
    key: crate::vertex::ServiceAccount,
    model: &str,
    protocol: VertexProtocol,
    listing: Vec<u8>,
    clock: &(dyn grapt_types::ClockAuthority + Sync),
) -> Result<VerifiedKey, KeyError> {
    if listing.len() > 4 * 1024 * 1024 {
        return Err(KeyError::Invalid);
    }
    let binding =
        DestinationBinding::key(ApiDestination::parse(base).map_err(|_| KeyError::Invalid)?);
    let token = crate::vertex::exchange(&key.0, clock)
        .await
        .map_err(|_| KeyError::Refused)?;
    let secret = VaultSecret::vertex(key.0, token).map_err(|_| KeyError::Invalid)?;
    let verified = VerifiedKey {
        cloudflare: None,
        base: base.to_owned(),
        secret,
        binding,
        listing,
        watsonx_project: None,
    };
    match protocol {
        VertexProtocol::Gemini => google::verify_completion(base, &verified, model).await?,
        VertexProtocol::Anthropic => verify_claude(base, &verified, model).await?,
    }
    Ok(verified)
}

/// Verify native Vertex using the operator's application-default source.
pub async fn verify_vertex_adc(
    base: &str,
    model: &str,
    protocol: VertexProtocol,
    listing: Vec<u8>,
) -> Result<VerifiedKey, KeyError> {
    if listing.len() > 4 * 1024 * 1024 {
        return Err(KeyError::Invalid);
    }
    let binding =
        DestinationBinding::key(ApiDestination::parse(base).map_err(|_| KeyError::Invalid)?);
    let verified = VerifiedKey {
        cloudflare: None,
        base: base.to_owned(),
        secret: VaultSecret::vertex_adc(),
        binding,
        listing,
        watsonx_project: None,
    };
    match protocol {
        VertexProtocol::Gemini => google::verify_completion(base, &verified, model).await?,
        VertexProtocol::Anthropic => verify_claude(base, &verified, model).await?,
    }
    Ok(verified)
}

async fn verify_claude(base: &str, verified: &VerifiedKey, model: &str) -> Result<(), KeyError> {
    if model.is_empty()
        || model.len() > 256
        || !model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.@".contains(&b))
    {
        return Err(KeyError::Invalid);
    }
    let url = format!("{}/{model}:rawPredict", base.trim_end_matches('/'));
    verified
        .binding
        .check_api(&url)
        .map_err(|_| KeyError::Invalid)?;
    let client = crate::chatgpt::bounded_client(base.starts_with("https://"))
        .map_err(|_| KeyError::Unreachable)?;
    let request = match verified.secret.expose() {
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
    let value = google::document(
        request
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .body(
                serde_json::json!({
                    "anthropic_version":"vertex-2023-10-16",
                    "max_tokens":128,
                    "messages":[{"role":"user","content":"Reply with OK."}]
                })
                .to_string(),
            ),
    )
    .await?;
    if value["stop_reason"] != "end_turn"
        || !value["content"].as_array().is_some_and(|parts| {
            parts.iter().any(|part| {
                part["type"] == "text"
                    && part["text"]
                        .as_str()
                        .is_some_and(|text| !text.trim().is_empty())
            })
        })
    {
        return Err(KeyError::Refused);
    }
    Ok(())
}
