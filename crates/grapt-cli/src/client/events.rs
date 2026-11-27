//! Advisory subscription and explicit Session-scoped approval attachment.
use super::*;

impl ApiClient {
    pub async fn events(&self) -> Result<AdvisoryStream, ApiClientError> {
        self.events_with_approver(None).await
    }

    pub async fn events_with_approver(
        &self,
        session: Option<&SessionId>,
    ) -> Result<AdvisoryStream, ApiClientError> {
        let request = self.authorized(self.event_client.get(self.endpoint.route("api/event")?));
        let request = match session {
            Some(session) => request.header("x-grapt-approver", session.as_str()),
            None => request,
        };
        let response = request
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await
            .map_err(ApiClientError::Transport)?;
        if !response.status().is_success() {
            return Err(ApiClientError::Status(response.status()));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next());
        if content_type != Some("text/event-stream") {
            return Err(ApiClientError::EventContentType);
        }
        Ok(AdvisoryStream {
            response,
            buffered: Vec::new(),
            pending: VecDeque::new(),
            limits: self.limits,
            skipped_kinds: 0,
            consecutive_skips: 0,
        })
    }
}

#[cfg(test)]
mod tests;
