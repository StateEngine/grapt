//! Verify native Bedrock bearer credentials with an actual Converse completion.
use super::{KeyError, VerifiedKey};
use crate::vault::{ApiDestination, CredentialView, DestinationBinding, VaultSecret};

#[expect(clippy::too_many_lines, reason = "legacy")]
pub async fn verify(
    base: &str,
    model: &str,
    secret: VaultSecret,
    listing: Vec<u8>,
    clock: &(dyn grapt_types::ClockAuthority + Sync),
) -> Result<VerifiedKey, KeyError> {
    let binding =
        DestinationBinding::key(ApiDestination::parse(base).map_err(|_| KeyError::Invalid)?);
    let mut url = reqwest::Url::parse(base).map_err(|_| KeyError::Invalid)?;
    if model.is_empty() || model.len() > 256 || model.chars().any(char::is_control) {
        return Err(KeyError::Invalid);
    }
    url.path_segments_mut()
        .map_err(|_| KeyError::Invalid)?
        .pop_if_empty()
        .push("model")
        .push(model)
        .push("converse");
    binding
        .check_api(url.as_str())
        .map_err(|_| KeyError::Invalid)?;
    let body = serde_json::json!({"messages":[{"role":"user","content":[{"text":"Reply OK."}]}],"inferenceConfig":{"maxTokens":16}}).to_string();
    let client = crate::chatgpt::bounded_client(base.starts_with("https://"))
        .map_err(|_| KeyError::Unreachable)?;
    let request = client
        .post(url)
        .header("content-type", "application/json")
        .body(body);
    let aws_chain = matches!(secret.expose(), CredentialView::AwsChain(_));
    let request = match secret.expose() {
        CredentialView::ApiKey(key) => request
            .bearer_auth(key)
            .build()
            .map_err(|_| KeyError::Invalid)?,
        CredentialView::AwsChain(selection) => {
            let request = request.build().map_err(|_| KeyError::Invalid)?;
            selection
                .sign(request, clock)
                .await
                .map_err(|error| match error {
                    crate::aws::AwsError::ProfileConfiguration => KeyError::AwsProfileConfiguration,
                    _ => KeyError::AwsCredentials,
                })?
        }
        _ => return Err(KeyError::Invalid),
    };
    let mut response = client
        .execute(request)
        .await
        .map_err(|_| KeyError::Unreachable)?;
    if matches!(response.status().as_u16(), 401 | 403) {
        return Err(if aws_chain {
            KeyError::AwsCredentials
        } else {
            KeyError::Refused
        });
    }
    if !response.status().is_success() {
        return Err(KeyError::Listing);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| KeyError::Unreachable)? {
        if chunk.len() > (256 * 1024usize).saturating_sub(bytes.len()) {
            return Err(KeyError::Listing);
        }
        bytes.extend_from_slice(&chunk);
    }
    let result: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| KeyError::Listing)?;
    if result["output"]["message"]["role"] != "assistant"
        || !result["output"]["message"]["content"]
            .as_array()
            .is_some_and(|rows| {
                rows.iter().any(|row| {
                    row["text"].as_str().is_some_and(|text| !text.is_empty())
                        || row["reasoningContent"]["reasoningText"]["text"]
                            .as_str()
                            .is_some()
                        || row["reasoningContent"]["redactedContent"]
                            .as_str()
                            .is_some_and(|data| !data.is_empty())
                })
            })
        || !matches!(
            result["stopReason"].as_str(),
            Some("end_turn" | "stop_sequence" | "max_tokens")
        )
    {
        return Err(KeyError::Listing);
    }
    let rows: serde_json::Value =
        serde_json::from_slice(&listing).map_err(|_| KeyError::Listing)?;
    if listing.len() > 4 * 1024 * 1024
        || !rows["data"]
            .as_array()
            .is_some_and(|rows| rows.len() == 1 && rows[0]["id"].as_str() == Some(model))
    {
        return Err(KeyError::Listing);
    }
    Ok(VerifiedKey {
        watsonx_project: None,
        base: base.to_owned(),
        secret,
        binding,
        listing,
        cloudflare: None,
    })
}
