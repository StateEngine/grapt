//! Explicit Azure CLI authorization for the selected native resource.
use azure_core::credentials::TokenCredential;
use azure_identity::{AzureCliCredential, AzureCliCredentialOptions};

mod executor;

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum AzureError {
    #[error("Microsoft Entra ID requires an HTTPS Azure resource endpoint")]
    Destination,
    #[error("Azure CLI authorization is unavailable; install Azure CLI 2.54 or newer, run az login for the service's user, check the selected resource permissions, then retry Connect")]
    Unavailable,
}

/// Validate resource coordinates without activating credential resolution.
pub fn validate_endpoint(endpoint: &str) -> Result<(), AzureError> {
    let url = reqwest::Url::parse(endpoint).map_err(|_| AzureError::Destination)?;
    scope(&url).map(|_| ())
}

// The resource determines the token audience before the CLI is invoked.
fn scope(url: &reqwest::Url) -> Result<&'static str, AzureError> {
    let host = url.host_str().ok_or(AzureError::Destination)?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
    {
        return Err(AzureError::Destination);
    }
    if host.ends_with(".services.ai.azure.com") {
        return Ok(if url.path().starts_with("/models") {
            "https://cognitiveservices.azure.com/.default"
        } else {
            "https://ai.azure.com/.default"
        });
    }
    if host.ends_with(".openai.azure.com") || host.ends_with(".cognitiveservices.azure.com") {
        return Ok("https://cognitiveservices.azure.com/.default");
    }
    Err(AzureError::Destination)
}

/// Resolve rotating tokens through the SDK; no token enters persistent storage.
pub async fn authorize(mut request: reqwest::Request) -> Result<reqwest::Request, AzureError> {
    let scope = scope(request.url())?;
    let credential = AzureCliCredential::new(Some(AzureCliCredentialOptions {
        executor: Some(std::sync::Arc::new(executor::Bounded)),
        ..Default::default()
    }))
    .map_err(|_| AzureError::Unavailable)?;
    let token = credential
        .get_token(&[scope], None)
        .await
        .map_err(|_| AzureError::Unavailable)?;
    let access = token.token.secret();
    if access.is_empty()
        || access.len() > 16 * 1024
        || !access.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(AzureError::Unavailable);
    }
    let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {access}"))
        .map_err(|_| AzureError::Unavailable)?;
    value.set_sensitive(true);
    request.headers_mut().remove("api-key");
    request.headers_mut().remove("x-api-key");
    request
        .headers_mut()
        .insert(reqwest::header::AUTHORIZATION, value);
    Ok(request)
}
