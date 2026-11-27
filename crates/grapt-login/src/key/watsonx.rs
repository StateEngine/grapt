//! IAM plus a completed native generation are required before installation.
use super::*;

pub async fn verify_watsonx(
    base: &str,
    key: crate::watsonx::ApiKey,
    project: grapt_types::WatsonxProjectId,
    model: &str,
    listing: Vec<u8>,
    clock: &(dyn grapt_types::ClockAuthority + Sync),
) -> Result<VerifiedKey, KeyError> {
    if listing.len() > 4 * 1024 * 1024
        || model.is_empty()
        || model.len() > 256
        || !model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._/".contains(&b))
    {
        return Err(KeyError::Invalid);
    }
    let binding =
        DestinationBinding::key(ApiDestination::parse(base).map_err(|_| KeyError::Invalid)?);
    let token = crate::watsonx::exchange(&key.0, clock)
        .await
        .map_err(|error| match error {
            crate::watsonx::WatsonxError::Unavailable => KeyError::Unreachable,
            crate::watsonx::WatsonxError::Invalid => KeyError::Invalid,
            crate::watsonx::WatsonxError::Authorization | crate::watsonx::WatsonxError::Expired => {
                KeyError::Refused
            }
        })?;
    let url = format!(
        "{}/ml/v1/text/chat?version={}",
        base.trim_end_matches('/'),
        grapt_types::WATSONX_API_VERSION
    );
    binding.check_api(&url).map_err(|_| KeyError::Invalid)?;
    let client = crate::chatgpt::bounded_client(base.starts_with("https://"))
        .map_err(|_| KeyError::Unreachable)?;
    let request = client.post(url).bearer_auth(token.access()).header("content-type", "application/json").body(serde_json::json!({"model_id":model,"project_id":project.as_str(),"messages":[{"role":"user","content":"Reply with OK."}],"max_tokens":2048}).to_string());
    let value = super::google::document(request).await?;
    let choices = value["choices"]
        .as_array()
        .filter(|rows| rows.len() == 1)
        .ok_or(KeyError::Refused)?;
    if choices[0]["finish_reason"] != "stop"
        || choices[0]["message"]["role"] != "assistant"
        || !choices[0]["message"]["content"]
            .as_str()
            .is_some_and(|text| !text.trim().is_empty())
    {
        return Err(KeyError::Refused);
    }
    let secret = VaultSecret::watsonx(key.0, token).map_err(|_| KeyError::Invalid)?;
    Ok(VerifiedKey {
        cloudflare: None,
        base: base.to_owned(),
        secret,
        binding,
        listing,
        watsonx_project: Some(project),
    })
}
