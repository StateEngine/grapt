//! Atomic startup selection uses the existing Session creation route.

use super::*;
use crate::StartupSelection;
use grapt_api::u3a::{DtoString, LocationRefDto, ModelRefDto};
use grapt_api::SessionCreateRequestDto;
use grapt_schema::tri_state_ledger::U3aSessionCreateLocation;

impl ApiClient {
    pub async fn create_session(&self) -> Result<SessionResponseDto, ApiClientError> {
        self.create_session_with(OptionalNullable::absent(), &StartupSelection::default())
            .await
    }

    /// Creates a Session at an explicit directory; the server resolves its project.
    pub async fn create_session_at(
        &self,
        directory: DtoString,
    ) -> Result<SessionResponseDto, ApiClientError> {
        self.create_session_at_selected(directory, &StartupSelection::default())
            .await
    }

    /// The server validates both choices before creating the Session.
    pub async fn create_session_at_selected(
        &self,
        directory: DtoString,
        selection: &StartupSelection,
    ) -> Result<SessionResponseDto, ApiClientError> {
        let location = LocationRefDto::new(Required::new(directory), Optional::absent());
        self.create_session_with(OptionalNullable::value(location), selection)
            .await
    }

    async fn create_session_with(
        &self,
        location: OptionalNullable<LocationRefDto, U3aSessionCreateLocation>,
        selection: &StartupSelection,
    ) -> Result<SessionResponseDto, ApiClientError> {
        let agent = match &selection.agent {
            Some(agent) => OptionalNullable::value(
                WireId::parse(agent.as_str().to_owned()).ok_or(ApiClientError::Encode)?,
            ),
            None => OptionalNullable::absent(),
        };
        let model = match &selection.model {
            Some(model) => OptionalNullable::value(ModelRefDto::new(
                Required::new(
                    WireId::parse(model.model.as_str().to_owned()).ok_or(ApiClientError::Encode)?,
                ),
                Required::new(
                    WireId::parse(model.provider.as_str().to_owned())
                        .ok_or(ApiClientError::Encode)?,
                ),
                Optional::absent(),
            )),
            None => OptionalNullable::absent(),
        };
        let body = SessionCreateRequestDto::new(OptionalNullable::absent(), agent, model, location);
        // The headless CLI is a launcher that may legitimately run with no
        // window. Stating that here is what stops a window which later attaches
        // and leaves from ending the Session: the server records the mode at
        // admission. This client never runs a window of its own, so it states
        // `headless`; a client that does run one states `interactive`.
        self.post_create(grapt_types::SessionLaunch::Headless, &body)
            .await
    }
}

impl ApiClient {
    /// `POST api/session` carrying the launch mode the creating client states.
    ///
    /// Separate from the shared `send_json` so those callers need not name a
    /// mode they have none of; here the header is the whole point of the call.
    async fn post_create<B>(
        &self,
        launch: grapt_types::SessionLaunch,
        body: &B,
    ) -> Result<SessionResponseDto, ApiClientError>
    where
        B: Serialize + ?Sized,
    {
        let bytes = serde_json::to_vec(body).map_err(|_| ApiClientError::Encode)?;
        if bytes.len() > self.limits.values().stdin_bytes {
            return Err(ApiClientError::ResponseTooLarge {
                maximum: self.limits.values().stdin_bytes,
            });
        }
        let request = self
            .request(Method::POST, "api/session")?
            .header("x-grapt-launch", launch.as_str())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(bytes);
        let response = request.send().await.map_err(ApiClientError::Transport)?;
        let response = json_response(response).await?;
        let body = bounded_body(response, MAX_RESPONSE_BODY_BYTES).await?;
        serde_json::from_slice(&body).map_err(|_| ApiClientError::Decode)
    }
}
