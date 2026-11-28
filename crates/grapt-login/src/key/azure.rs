//! Azure OpenAI v1 uses the native Responses route and OpenAI-client key flow.
//! https://learn.microsoft.com/azure/foundry/openai/api-version-lifecycle
use super::{KeyError, KeyProtocol, VerifiedKey};
use crate::vault::{ApiDestination, DestinationBinding, VaultSecret};

pub fn api_base(endpoint: &str) -> Result<String, KeyError> {
    ApiDestination::parse(endpoint).map_err(|_| KeyError::Invalid)?;
    let mut url = reqwest::Url::parse(endpoint).map_err(|_| KeyError::Invalid)?;
    if !matches!(
        url.path().trim_end_matches('/'),
        "" | "/openai" | "/openai/v1"
    ) {
        return Err(KeyError::Invalid);
    }
    url.set_path("/openai/v1");
    Ok(url.into())
}

/// A deployment completion authenticates the selected model. A resource's
/// model catalog cannot establish the deployment name or permission to use it.
pub async fn verify(
    api_base: &str,
    secret: VaultSecret,
    deployment: &str,
    listing: Vec<u8>,
) -> Result<VerifiedKey, KeyError> {
    let binding =
        DestinationBinding::key(ApiDestination::parse(api_base).map_err(|_| KeyError::Invalid)?);
    if listing.len() > 4 * 1024 * 1024 {
        return Err(KeyError::Listing);
    }
    let rows: serde_json::Value =
        serde_json::from_slice(&listing).map_err(|_| KeyError::Listing)?;
    if deployment.is_empty()
        || deployment.len() > 256
        || deployment.chars().any(char::is_control)
        || !rows["data"]
            .as_array()
            .is_some_and(|rows| rows.len() == 1 && rows[0]["id"].as_str() == Some(deployment))
    {
        return Err(KeyError::Listing);
    }
    let verified = VerifiedKey {
        watsonx_project: None,
        base: api_base.to_owned(),
        secret,
        binding,
        listing,
        cloudflare: None,
    };
    if matches!(
        verified.secret.expose(),
        crate::vault::CredentialView::AzureCli
    ) {
        let client = crate::chatgpt::bounded_client(true).map_err(|_| KeyError::Unreachable)?;
        let request = client.post(format!("{api_base}/responses"))
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&serde_json::json!({"model":deployment,"input":"Hello","max_output_tokens":32,"stream":false})).map_err(|_| KeyError::Invalid)?)
            .build().map_err(|_| KeyError::Invalid)?;
        let request = crate::azure::authorize(request)
            .await
            .map_err(|_| KeyError::AzureCredentials)?;
        let response =
            super::google::document(reqwest::RequestBuilder::from_parts(client, request))
                .await
                .map_err(|_| KeyError::AzureCredentials)?;
        if response["error"].is_null()
            && response["output"].is_array()
            && matches!(
                response["status"].as_str(),
                Some("completed" | "incomplete")
            )
        {
            Ok(verified)
        } else {
            Err(KeyError::Completion)
        }
    } else {
        verified
            .check_model(api_base, KeyProtocol::Responses, deployment)
            .await?;
        Ok(verified)
    }
}
