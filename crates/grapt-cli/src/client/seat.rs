use super::*;
use grapt_api::u3a::{InstructionEntryPutDto, SessionCapabilityPutDto};

impl ApiClient {
    pub(crate) async fn create_seat(
        &self,
        body: &SessionCreateRequestDto,
    ) -> Result<SessionResponseDto, ApiClientError> {
        self.send_json(Method::POST, "api/session", Some(body))
            .await
    }

    pub(crate) async fn put_seat_entry(
        &self,
        session: &SessionId,
        key: &str,
        body: &InstructionEntryPutDto,
    ) -> Result<(), ApiClientError> {
        self.send_no_content(
            Method::PUT,
            &format!("api/session/{session}/instructions/entries/{key}"),
            body,
        )
        .await
    }

    pub(crate) async fn put_seat_capability(
        &self,
        session: &SessionId,
        body: &SessionCapabilityPutDto,
    ) -> Result<(), ApiClientError> {
        self.send_no_content(
            Method::PUT,
            &format!("api/session/{session}/stateengine/capability"),
            body,
        )
        .await
    }
}
