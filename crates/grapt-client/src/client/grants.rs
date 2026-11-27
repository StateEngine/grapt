//! The inventory is a projection of the authenticated service, never authority.
use super::*;
use grapt_api::u3a::{
    SandboxGrantDto, SandboxGrantListResponseDto, SandboxGrantRevokeResponseDto,
    SandboxGrantRevokeStateDto,
};

#[derive(Debug)]
pub(super) struct State {
    session: Option<SessionWireId>,
    rows: Result<Vec<SandboxGrantDto>, String>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            session: None,
            rows: Ok(Vec::new()),
        }
    }
}

impl<C: Connection> Client<C> {
    pub fn sandbox_grants(&self) -> Result<Option<&[SandboxGrantDto]>, &str> {
        if self.grants.session.as_ref() != self.selected.as_ref() || self.grants.session.is_none() {
            return Ok(None);
        }
        self.grants
            .rows
            .as_ref()
            .map(|rows| Some(rows.as_slice()))
            .map_err(String::as_str)
    }

    pub fn refresh_sandbox_grants(&mut self) -> Result<(), String> {
        let session = self
            .selected
            .clone()
            .ok_or("Choose a session before inspecting permissions")?;
        let result = self
            .ingest(
                Operation::SandboxGrantList,
                Some(&session),
                None,
                None,
                None,
                None,
            )
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                serde_json::from_slice::<SandboxGrantListResponseDto>(&bytes)
                    .map_err(|error| error.to_string())
            })
            .map(|response| response.data().get().as_slice().to_vec());
        self.grants.session = Some(session);
        self.grants.rows = result;
        self.grants.rows.as_ref().map(|_| ()).map_err(Clone::clone)
    }

    pub fn revoke_sandbox_grant(&mut self, grant: &SandboxGrantDto) -> Result<String, String> {
        let session = self
            .selected
            .clone()
            .ok_or("Choose a session before revoking a grant")?;
        let body = serde_json::to_vec(grant).map_err(|error| error.to_string())?;
        let mut message = String::new();
        // Each server request is bounded; large legacy rules may require more
        // than one event. Never report completion from a pending response.
        for _ in 0..4 {
            let bytes = self
                .ingest(
                    Operation::SandboxGrantRevoke,
                    Some(&session),
                    None,
                    Some(body.clone()),
                    None,
                    None,
                )
                .map_err(|error| error.to_string())?;
            let response: SandboxGrantRevokeResponseDto =
                serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            message = response.message().get().as_str().to_owned();
            if *response.state().get() == SandboxGrantRevokeStateDto::Complete {
                break;
            }
        }
        match self.refresh_sandbox_grants() {
            Ok(()) => Ok(message),
            Err(error) => Ok(format!("{message} Grant list refresh failed: {error}")),
        }
    }
}
