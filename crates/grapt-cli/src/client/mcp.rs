//! MCP operator requests use the ordinary authenticated API transport.
use super::{bounded_body, json_response, ApiClient, ApiClientError, Method};
use grapt_api::u3a::{McpAuthRequestDto, McpAuthResponseDto, McpAuthState, McpListResponseDto};
use std::time::Duration;

impl ApiClient {
    pub(crate) async fn mcp_configuration_list(
        &self,
        directory: Option<&str>,
    ) -> Result<grapt_api::u3a::McpConfigurationListResponseDto, ApiClientError> {
        let mut url = self.endpoint.route("api/mcp/configuration")?;
        if let Some(directory) = directory {
            url.query_pairs_mut()
                .append_pair("location[directory]", directory);
        }
        let response = self
            .request(Method::GET, url.as_str())?
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(ApiClientError::Transport)?;
        let body = bounded_body(json_response(response).await?, 1024 * 1024).await?;
        serde_json::from_slice(&body).map_err(|_| ApiClientError::Decode)
    }

    pub(crate) async fn mcp_list(
        &self,
        directory: Option<&str>,
    ) -> Result<McpListResponseDto, ApiClientError> {
        let mut url = self.endpoint.route("api/mcp")?;
        if let Some(directory) = directory {
            url.query_pairs_mut()
                .append_pair("location[directory]", directory);
        }
        let response = self
            .request(Method::GET, url.as_str())?
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(ApiClientError::Transport)?;
        let body = bounded_body(json_response(response).await?, 1024 * 1024).await?;
        serde_json::from_slice(&body).map_err(|_| ApiClientError::Decode)
    }

    pub(crate) async fn mcp_configuration(
        &self,
        server: &str,
        directory: Option<&str>,
        request: &grapt_api::u3a::McpConfigurationRequestDto,
    ) -> Result<(), ApiClientError> {
        let mut url = self.endpoint.route("api/mcp/")?;
        url.path_segments_mut()
            .map_err(|_| ApiClientError::InvalidEndpoint)?
            .pop_if_empty()
            .push(server)
            .push("configuration");
        if let Some(directory) = directory {
            url.query_pairs_mut()
                .append_pair("location[directory]", directory);
        }
        let body = serde_json::to_vec(request).map_err(|_| ApiClientError::Encode)?;
        let response = self
            .request(Method::POST, url.as_str())?
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(ApiClientError::Transport)?;
        let response = json_response(response).await?;
        if response.status() != reqwest::StatusCode::NO_CONTENT {
            return Err(ApiClientError::Status(response.status()));
        }
        if bounded_body(response, 1).await?.is_empty() {
            Ok(())
        } else {
            Err(ApiClientError::Decode)
        }
    }

    pub(crate) async fn mcp_auth(
        &self,
        server: &str,
        directory: Option<&str>,
        request: &McpAuthRequestDto,
    ) -> Result<McpAuthResponseDto, ApiClientError> {
        if server.is_empty()
            || server.len() > 256
            || server.chars().any(char::is_control)
            || matches!(server, "." | "..")
        {
            return Err(ApiClientError::InvalidEndpoint);
        }
        let component: String = server.bytes().map(|byte| format!("%{byte:02X}")).collect();
        let mut url = self.endpoint.route(&format!("api/mcp/{component}/auth"))?;
        if let Some(directory) = directory {
            url.query_pairs_mut()
                .append_pair("location[directory]", directory);
        }
        let body = serde_json::to_vec(request).map_err(|_| ApiClientError::Encode)?;
        if body.len() > 64 * 1024 {
            return Err(ApiClientError::Encode);
        }
        let response = self
            .request(Method::POST, url.as_str())?
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .timeout(Duration::from_secs(80))
            .body(body)
            .send()
            .await
            .map_err(ApiClientError::Transport)?;
        let body = bounded_body(json_response(response).await?, 64 * 1024).await?;
        let response: McpAuthResponseDto =
            serde_json::from_slice(&body).map_err(|_| ApiClientError::Decode)?;
        let valid = match request {
            McpAuthRequestDto::Login { .. } => {
                response.state == McpAuthState::AwaitingBrowser
                    && response.login_id.is_some()
                    && response.authorization_url.is_some()
            }
            McpAuthRequestDto::Status { login_id } => {
                response.login_id.as_ref() == Some(login_id)
                    && response.state != McpAuthState::SignedOut
            }
            McpAuthRequestDto::Logout {} => response.state == McpAuthState::SignedOut,
        };
        if valid {
            Ok(response)
        } else {
            Err(ApiClientError::Decode)
        }
    }
}

#[cfg(test)]
mod tests;
