//! Connection operators use the existing authenticated local API transport.
use super::{ApiClient, ApiClientError, ClientMode, Method};
use grapt_api::u3a::{
    CredentialIdDto, IntegrationAttemptIdDto, IntegrationIdDto, IntegrationKeyConnectRequestDto,
    IntegrationOAuthConnectRequestDto, IntegrationOAuthConnectResponseDto,
    IntegrationOAuthStatusResponseDto,
};

/// The service's own provider checks bound a connect: its model list and the
/// completion check on a loading local model, with room for the reads after.
const CONNECT_ALLOWANCE: std::time::Duration = grapt_api::u3a::INTEGRATION_KEY_CONNECT_WAIT;

impl ApiClient {
    pub(crate) async fn authentication_status(
        &self,
    ) -> Result<grapt_api::u3a::AuthenticationStatusResponseDto, ApiClientError> {
        let path = self.connection_route(&["auth", "status"])?;
        self.send_json::<(), _>(Method::GET, &path, None).await
    }

    // This local extension selects a connection without changing retained DTOs.
    fn connection_request<B: serde::Serialize>(
        &self,
        path: &str,
        connection: Option<&str>,
        body: &B,
    ) -> Result<reqwest::RequestBuilder, ApiClientError> {
        let bytes = serde_json::to_vec(body).map_err(|_| ApiClientError::Encode)?;
        if bytes.len() > self.limits.values().stdin_bytes {
            return Err(ApiClientError::ResponseTooLarge {
                maximum: self.limits.values().stdin_bytes,
            });
        }
        // A connect checks a completion on the provider before it answers,
        // and a server on this machine may load the model first: the
        // ordinary request allowance would cut that answer off.
        let mut request = self
            .request(Method::POST, path)?
            .timeout(CONNECT_ALLOWANCE)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(bytes);
        if let Some(connection) = connection {
            request = request.header("x-grapt-connection-id", connection);
        }
        Ok(request)
    }
    async fn connection_no_content<B: serde::Serialize>(
        &self,
        path: &str,
        connection: Option<&str>,
        body: &B,
        diagnostic_key: Option<&str>,
    ) -> Result<(), ApiClientError> {
        let response = self
            .connection_request(path, connection, body)?
            .send()
            .await
            .map_err(ApiClientError::Transport)?;
        if !response.status().is_success() {
            let status = response.status();
            if status == reqwest::StatusCode::BAD_REQUEST {
                let bytes = super::bounded_body(response, super::MAX_ERROR_BODY_BYTES)
                    .await
                    .map_err(|error| ApiClientError::StatusDetail {
                        status,
                        detail: format!("error response could not be read: {error}"),
                    })?;
                if let Ok(error) =
                    serde_json::from_slice::<grapt_api::u3a::InvalidRequestErrorDto>(&bytes)
                {
                    if error.kind().as_value().is_some_and(|kind| {
                        kind.as_str() == grapt_api::u3a::INTEGRATION_AZURE_CREDENTIALS_KIND
                    }) {
                        return Err(ApiClientError::AzureCredentials);
                    }
                    if error.kind().as_value().is_some_and(|kind| {
                        kind.as_str() == grapt_api::u3a::INTEGRATION_KEY_REJECTED_KIND
                    }) {
                        return Err(ApiClientError::ProviderKeyRejected);
                    }
                    if error.kind().as_value().is_some_and(|kind| {
                        kind.as_str() == grapt_api::u3a::INTEGRATION_ENDPOINT_KIND
                    }) {
                        let message = error.message().get().as_str();
                        if !message.is_empty()
                            && message.len() <= 1024
                            && !message.chars().any(char::is_control)
                        {
                            return Err(ApiClientError::EndpointRefused(message.to_owned()));
                        }
                    }
                    if error
                        .kind()
                        .as_value()
                        .is_some_and(|kind| kind.as_str() == "integration_authorization")
                    {
                        return Err(ApiClientError::ProviderVerificationFailed);
                    }
                    if error.kind().as_value().is_some_and(|kind| {
                        kind.as_str() == grapt_api::u3a::INTEGRATION_GOOGLE_ADC_KIND
                    }) {
                        return Err(ApiClientError::GoogleAdcCredentials);
                    }
                    if error.kind().as_value().is_some_and(|kind| {
                        kind.as_str() == grapt_api::u3a::INTEGRATION_AWS_CREDENTIALS_KIND
                    }) {
                        return Err(ApiClientError::AwsCredentials);
                    }
                    if error.kind().as_value().is_some_and(|kind| {
                        kind.as_str() == grapt_api::u3a::INTEGRATION_AWS_PROFILE_KIND
                    }) {
                        return Err(ApiClientError::AwsProfileConfiguration);
                    }
                    if let Some(key) = diagnostic_key.filter(|_| cfg!(feature = "loopback-testkit"))
                    {
                        let message = error.message().get().clone().into_string();
                        let detail = if key.is_empty() {
                            message
                        } else {
                            message.replace(key, "[redacted]")
                        };
                        let detail = if detail.is_empty()
                            || detail.len() > 1024
                            || detail.chars().any(char::is_control)
                        {
                            "local connection diagnostic is empty, exceeds 1024 bytes, or contains control characters".into()
                        } else {
                            detail
                        };
                        return Err(ApiClientError::StatusDetail { status, detail });
                    }
                }
                return Err(ApiClientError::Status(status));
            }
            return Err(ApiClientError::Status(response.status()));
        }
        if super::bounded_body(response, 1).await?.is_empty() {
            Ok(())
        } else {
            Err(ApiClientError::Decode)
        }
    }
    /// Keys and sign-in attempts never leave the attributed loopback service.
    fn connection_route(&self, segments: &[&str]) -> Result<String, ApiClientError> {
        if self.endpoint.mode == ClientMode::RemoteTls {
            return Err(ApiClientError::InvalidEndpoint);
        }
        let mut url = self.endpoint.route("api/")?;
        url.path_segments_mut()
            .map_err(|_| ApiClientError::InvalidEndpoint)?
            .pop_if_empty()
            .extend(segments.iter().copied());
        Ok(url.into())
    }

    pub(crate) async fn integration_key(
        &self,
        provider: &IntegrationIdDto,
        connection: Option<&str>,
        body: &IntegrationKeyConnectRequestDto,
    ) -> Result<(), ApiClientError> {
        let path = self.connection_route(&["integration", provider.as_str(), "connect", "key"])?;
        self.connection_no_content(&path, connection, body, Some(body.key.as_str()))
            .await
    }

    pub(crate) async fn integration_environment(
        &self,
        provider: &IntegrationIdDto,
        connection: Option<&str>,
        body: &grapt_api::u3a::IntegrationEnvironmentConnectRequestDto,
    ) -> Result<(), ApiClientError> {
        let path = self.connection_route(&["integration", provider.as_str(), "connect", "env"])?;
        self.connection_no_content(&path, connection, body, None)
            .await
    }

    pub(crate) async fn integration_oauth_challenge(
        &self,
        provider: &IntegrationIdDto,
        attempt: &IntegrationAttemptIdDto,
    ) -> Result<grapt_api::u3a::IntegrationOAuthChallengeResponseDto, ApiClientError> {
        let path = self.connection_route(&[
            "integration",
            provider.as_str(),
            "connect",
            "oauth",
            attempt.as_inner().as_str(),
            "challenge",
        ])?;
        self.send_json::<(), _>(Method::GET, &path, None).await
    }

    pub(crate) async fn integration_oauth(
        &self,
        provider: &IntegrationIdDto,
        connection: Option<&str>,
        body: &IntegrationOAuthConnectRequestDto,
    ) -> Result<IntegrationOAuthConnectResponseDto, ApiClientError> {
        let path =
            self.connection_route(&["integration", provider.as_str(), "connect", "oauth"])?;
        let response = self
            .connection_request(&path, connection, body)?
            .send()
            .await
            .map_err(ApiClientError::Transport)?;
        let response = super::json_response(response).await?;
        let bytes = super::bounded_body(response, super::MAX_RESPONSE_BODY_BYTES).await?;
        serde_json::from_slice(&bytes).map_err(|_| ApiClientError::Decode)
    }

    pub(crate) async fn integration_oauth_status(
        &self,
        provider: &IntegrationIdDto,
        attempt: &IntegrationAttemptIdDto,
    ) -> Result<IntegrationOAuthStatusResponseDto, ApiClientError> {
        let path = self.connection_route(&[
            "integration",
            provider.as_str(),
            "connect",
            "oauth",
            attempt.as_inner().as_str(),
        ])?;
        self.send_json::<(), _>(Method::GET, &path, None).await
    }

    pub(crate) async fn integration_oauth_cancel(
        &self,
        provider: &IntegrationIdDto,
        attempt: &IntegrationAttemptIdDto,
    ) -> Result<(), ApiClientError> {
        let path = self.connection_route(&[
            "integration",
            provider.as_str(),
            "connect",
            "oauth",
            attempt.as_inner().as_str(),
        ])?;
        self.delete_empty(&path).await
    }

    pub(crate) async fn credential_remove(
        &self,
        credential: &CredentialIdDto,
    ) -> Result<(), ApiClientError> {
        let path = self.connection_route(&["credential", credential.as_str()])?;
        self.delete_empty(&path).await
    }
}
