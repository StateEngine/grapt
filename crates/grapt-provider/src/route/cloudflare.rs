//! Closed gateway authentication and exact endpoint scope.
use super::*;
use crate::CredentialPlacement;
use grapt_types::cloudflare::{CloudflareGatewayScope, CloudflareWire};

pub fn credential_placement(
    family: ProtocolFamily,
    scope: Option<&CloudflareGatewayScope>,
    auth: crate::MessagesAuthMode,
) -> Option<CredentialPlacement> {
    let Some(scope) = scope else {
        return crate::messages_placement(family, auth);
    };
    if auth != crate::MessagesAuthMode::ApiKey {
        return None;
    }
    match (scope.wire(), family) {
        (CloudflareWire::Responses, ProtocolFamily::OpenAiResponses)
        | (CloudflareWire::Messages, ProtocolFamily::AnthropicMessages) => {
            Some(CredentialPlacement::CloudflareGateway)
        }
        (CloudflareWire::Chat, ProtocolFamily::OpenAiCompatible) => {
            Some(CredentialPlacement::BearerAuthorization)
        }
        _ => None,
    }
}

pub(super) fn check(
    scope: &CloudflareGatewayScope,
    base: &str,
    family: ProtocolFamily,
) -> Result<(), RouteConfigError> {
    if credential_placement(family, Some(scope), crate::MessagesAuthMode::ApiKey).is_none() {
        return Err(RouteConfigError::CloudflareScopeMismatch);
    }
    if base == scope.api_base() {
        return Ok(());
    }
    #[cfg(feature = "loopback-testkit")]
    {
        let url =
            reqwest::Url::parse(base).map_err(|_| RouteConfigError::CloudflareScopeMismatch)?;
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
    Err(RouteConfigError::CloudflareScopeMismatch)
}

/// Credential owners authenticate only their native provider routes.
pub fn credential_can_authenticate(
    family: ProtocolFamily,
    scope: Option<&CloudflareGatewayScope>,
    auth: crate::MessagesAuthMode,
    placement: CredentialPlacement,
) -> bool {
    if placement == CredentialPlacement::VertexAdc {
        matches!(
            family,
            ProtocolFamily::VertexGemini | ProtocolFamily::VertexAnthropic
        ) && scope.is_none()
            && auth == crate::MessagesAuthMode::ApiKey
    } else if placement == CredentialPlacement::AwsSigV4 {
        family == ProtocolFamily::BedrockConverse
            && scope.is_none()
            && auth == crate::MessagesAuthMode::ApiKey
    } else if placement == CredentialPlacement::AzureEntra {
        family == ProtocolFamily::OpenAiResponses
            && scope.is_none()
            && auth == crate::MessagesAuthMode::ApiKey
    } else {
        credential_placement(family, scope, auth) == Some(placement)
    }
}
