use super::{bounded_body, json_response, ApiClient, ApiClientError, Method};
use grapt_api::u3a::{ClaudePluginRequestDto, ClaudePluginResponseDto};
impl ApiClient {
    pub async fn claude_plugin(
        &self,
        directory: &str,
        request: &ClaudePluginRequestDto,
    ) -> Result<ClaudePluginResponseDto, ApiClientError> {
        let mut url = self.endpoint.route("api/plugin/claude")?;
        url.query_pairs_mut()
            .append_pair("location[directory]", directory);
        let response = self
            .request(Method::POST, url.as_str())?
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(request).map_err(|_| ApiClientError::Encode)?)
            .timeout(grapt_api::u3a::CLAUDE_PLUGIN_ALLOWANCE)
            .send()
            .await
            .map_err(ApiClientError::Transport)?;
        let body = bounded_body(json_response(response).await?, 65536).await?;
        serde_json::from_slice(&body).map_err(|_| ApiClientError::Decode)
    }
}
