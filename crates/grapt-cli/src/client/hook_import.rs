//! Hook imports use authenticated product validation and the established writer.
use super::{bounded_body, json_response, ApiClient, ApiClientError, Method};
use grapt_api::u3a::{HookImportRequestDto, HookImportResponseDto};
impl ApiClient {
    pub(crate) async fn import_hooks(
        &self,
        directory: &str,
        request: &HookImportRequestDto,
    ) -> Result<HookImportResponseDto, ApiClientError> {
        let mut url = self.endpoint.route("api/config/import-hooks")?;
        url.query_pairs_mut()
            .append_pair("location[directory]", directory);
        let response = self
            .request(Method::POST, url.as_str())?
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(request).map_err(|_| ApiClientError::Encode)?)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(ApiClientError::Transport)?;
        let body = bounded_body(json_response(response).await?, 65_536).await?;
        serde_json::from_slice(&body).map_err(|_| ApiClientError::Decode)
    }
}
