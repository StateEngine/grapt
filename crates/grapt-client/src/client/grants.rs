//! The inventory is a projection of the authenticated service, never authority.
use super::prepared::{Accepted, Done, PreparedCall, Revoke, Route, Step};
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
        let result = self.ingest(
            Operation::SandboxGrantList,
            Some(&session),
            None,
            None,
            None,
            None,
        );
        self.install_grants(session, result)
    }

    /// Re-read the selected Session's grants off the loop.
    pub fn prepare_grants_refresh(&mut self) -> Result<PreparedCall, ClientError> {
        let session = self.selected.clone().ok_or(ClientError::NoSelection)?;
        let route = Route::new(Operation::SandboxGrantList, Some(&session));
        self.prepare_call(Step::Grants(session.clone()), route)
    }

    /// The outcome, readable or not, is the inventory's: it is recorded and
    /// read back through [`Client::sandbox_grants`].
    pub(super) fn accept_grants(
        &mut self,
        session: SessionWireId,
        answer: Result<Vec<u8>, ClientError>,
    ) -> Accepted {
        let _ = self.install_grants(session, answer);
        Accepted::Done(Done::Applied)
    }

    fn install_grants(
        &mut self,
        session: SessionWireId,
        answer: Result<Vec<u8>, ClientError>,
    ) -> Result<(), String> {
        let result = answer
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
        for _ in 0..REVOKE_ROUNDS {
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
            let complete;
            (message, complete) = revoked(&bytes).map_err(|error| error.to_string())?;
            if complete {
                break;
            }
        }
        match self.refresh_sandbox_grants() {
            Ok(()) => Ok(message),
            Err(error) => Ok(format!("{message} Grant list refresh failed: {error}")),
        }
    }

    /// Revoke off the loop: each round is one step, until the service says
    /// the revocation is complete or the rounds run out.
    pub fn prepare_grant_revoke(
        &mut self,
        grant: &SandboxGrantDto,
    ) -> Result<PreparedCall, ClientError> {
        let session = self.selected.clone().ok_or(ClientError::NoSelection)?;
        let body = serde_json::to_vec(grant).map_err(|error| ClientError::Body {
            operation: "sandbox grant revoke",
            reason: error.to_string(),
        })?;
        self.revoke_round(Revoke {
            session,
            body,
            round: 1,
        })
    }

    pub(super) fn accept_revoke(
        &mut self,
        revoke: Revoke,
        answer: Result<Vec<u8>, ClientError>,
    ) -> Result<Accepted, ClientError> {
        let (message, complete) = revoked(&answer?)?;
        if complete || revoke.round >= REVOKE_ROUNDS {
            return Ok(Accepted::Done(Done::Revoked(message)));
        }
        self.revoke_round(Revoke {
            round: revoke.round + 1,
            ..revoke
        })
        .map(Accepted::Next)
    }

    fn revoke_round(&mut self, revoke: Revoke) -> Result<PreparedCall, ClientError> {
        let session = revoke.session.clone();
        let route = Route {
            body: Some(revoke.body.clone()),
            ..Route::new(Operation::SandboxGrantRevoke, Some(&session))
        };
        self.prepare_call(Step::Revoke(revoke), route)
    }
}

/// The most revocation requests one withdrawal sends.
const REVOKE_ROUNDS: u8 = 4;

/// The service's message on a revocation, and whether it is complete.
fn revoked(bytes: &[u8]) -> Result<(String, bool), ClientError> {
    let response: SandboxGrantRevokeResponseDto =
        serde_json::from_slice(bytes).map_err(|error| ClientError::Body {
            operation: "sandbox grant revoke",
            reason: error.to_string(),
        })?;
    let complete = *response.state().get() == SandboxGrantRevokeStateDto::Complete;
    Ok((response.message().get().as_str().to_owned(), complete))
}
