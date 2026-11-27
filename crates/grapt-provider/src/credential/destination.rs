//! Canonical API-origin binding carried by the opaque injected capability.
use super::CredentialError;

pub(super) fn cloudflare(
    scope: &grapt_types::cloudflare::CloudflareGatewayScope,
    url: &reqwest::Url,
) -> Result<(), CredentialError> {
    if url.as_str() == format!("{}{}", scope.api_base(), scope.request_path()) {
        return Ok(());
    }
    #[cfg(feature = "loopback-testkit")]
    if url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.port().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path() == format!("{}{}", scope.api_path(), scope.request_path())
    {
        return Ok(());
    }
    Err(CredentialError::DestinationMismatch)
}

pub(super) fn origin(raw: &str) -> Result<String, CredentialError> {
    if raw.len() > 2048 {
        return Err(CredentialError::InvalidDestination);
    }
    let url = reqwest::Url::parse(raw).map_err(|_| CredentialError::InvalidDestination)?;
    let loopback = url
        .host_str()
        .and_then(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .ok()
        })
        .is_some_and(|address| address.is_loopback());
    if !(url.scheme() == "https" || url.scheme() == "http" && loopback)
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(CredentialError::InvalidDestination);
    }
    Ok(url.origin().ascii_serialization())
}
