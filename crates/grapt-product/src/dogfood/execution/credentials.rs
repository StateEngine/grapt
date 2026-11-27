//! One serialized credential owner shared by ordinary and compaction attempts.
use std::sync::{Arc, Mutex};

use grapt_login::chatgpt::{self, Session, SignInClient};
use grapt_platform::{CredentialRefusal, ReloadableCredentialAuthority};
use grapt_provider::{
    CredentialOwner, CredentialRenewalFuture, CredentialRenewalRefusal, CredentialRevision,
    CurrentCredential, ProtocolFamily,
};
use grapt_types::ClockAuthority;

fn source_error(error: CredentialRefusal) -> CredentialRenewalRefusal {
    match error {
        CredentialRefusal::Absent | CredentialRefusal::EnvironmentAbsent { .. } => {
            CredentialRenewalRefusal::SourceAbsent
        }
        CredentialRefusal::RootMetadata(_)
        | CredentialRefusal::RootResolve(_)
        | CredentialRefusal::Metadata(_)
        | CredentialRefusal::Read(_) => CredentialRenewalRefusal::SourceUnreadable,
        CredentialRefusal::SourceChanged | CredentialRefusal::RootChanged => {
            CredentialRenewalRefusal::SourceChanged
        }
        CredentialRefusal::RenewalExhausted => CredentialRenewalRefusal::RevisionExhausted,
        CredentialRefusal::StaleCapability { .. } | CredentialRefusal::ForeignIssuance => {
            CredentialRenewalRefusal::CoordinatesChanged
        }
        _ => CredentialRenewalRefusal::SourceInvalid,
    }
}

fn sign_in_error(error: chatgpt::SignInError) -> CredentialRenewalRefusal {
    match error {
        chatgpt::SignInError::AccountChanged => CredentialRenewalRefusal::AccountChanged,
        chatgpt::SignInError::Expired(_) => CredentialRenewalRefusal::Expired,
        chatgpt::SignInError::Unfinished => CredentialRenewalRefusal::SourceAbsent,
        chatgpt::SignInError::ProfileChanged => CredentialRenewalRefusal::CoordinatesChanged,
        _ => CredentialRenewalRefusal::Unavailable,
    }
}

fn vertex_error(error: grapt_login::vertex::VertexError) -> CredentialRenewalRefusal {
    match error {
        grapt_login::vertex::VertexError::Expired => CredentialRenewalRefusal::Expired,
        grapt_login::vertex::VertexError::Invalid => CredentialRenewalRefusal::SourceInvalid,
        _ => CredentialRenewalRefusal::Unavailable,
    }
}

#[cfg(test)]
mod tests;

struct SignIn {
    session: Session,
    client: SignInClient,
    issuer: String,
}

enum CloudSession {
    Vertex(grapt_login::vertex::Session),
    Watsonx(grapt_login::watsonx::Session),
}
impl CloudSession {
    fn current(
        &self,
        source: &mut ReloadableCredentialAuthority,
    ) -> Result<CurrentCredential, CredentialRenewalRefusal> {
        match self {
            Self::Vertex(session) => source.current_vertex(session),
            Self::Watsonx(session) => source.current_watsonx(session),
        }
        .map_err(source_error)
    }
    async fn prepare(
        &mut self,
        clock: &(dyn ClockAuthority + Sync),
        allow: bool,
        force: bool,
    ) -> Result<bool, CredentialRenewalRefusal> {
        match self {
            Self::Vertex(session) => session
                .prepare(clock, allow, force)
                .await
                .map_err(vertex_error),
            Self::Watsonx(session) => {
                session
                    .prepare(clock, allow, force)
                    .await
                    .map_err(|e| match e {
                        grapt_login::watsonx::WatsonxError::Expired => {
                            CredentialRenewalRefusal::Expired
                        }
                        grapt_login::watsonx::WatsonxError::Invalid => {
                            CredentialRenewalRefusal::SourceInvalid
                        }
                        _ => CredentialRenewalRefusal::Unavailable,
                    })
            }
        }
    }
}
struct State {
    source: ReloadableCredentialAuthority,
    sign_in: Option<SignIn>,
    cloud: Option<CloudSession>,
    // A recovered replacement, or one minted before a request, must be
    // accepted before it may cause another mint.
    unaccepted_refresh: Option<CredentialRevision>,
    // A late old rejection must not re-arm a guard the provider already cleared.
    accepted_revision: Option<CredentialRevision>,
}

pub(super) struct LiveCredentials {
    state: Arc<Mutex<Option<State>>>,
    gate: Arc<tokio::sync::Semaphore>,
    clock: Arc<dyn ClockAuthority + Send + Sync>,
}

// One owned worker retains the slot through the bounded profile-lock wait and
// token request, including persistence if its caller times out or is cancelled.
// Taking and returning state use only short synchronous guards.
struct StateCustody {
    slot: Arc<Mutex<Option<State>>>,
    value: Option<State>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl Drop for StateCustody {
    fn drop(&mut self) {
        if let Some(state) = self.value.take() {
            let mut slot = match self.slot.lock() {
                Ok(slot) => slot,
                Err(poisoned) => poisoned.into_inner(),
            };
            *slot = Some(state);
        }
    }
}

impl LiveCredentials {
    async fn acquire_state(&self) -> Result<StateCustody, CredentialRenewalRefusal> {
        let permit = self
            .gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| CredentialRenewalRefusal::Unavailable)?;
        let value = {
            let mut slot = match self.state.lock() {
                Ok(slot) => slot,
                Err(poisoned) => poisoned.into_inner(),
            };
            slot.take().ok_or(CredentialRenewalRefusal::Unavailable)?
        };
        Ok(StateCustody {
            slot: self.state.clone(),
            value: Some(value),
            _permit: permit,
        })
    }

    pub(super) fn open(
        mut source: ReloadableCredentialAuthority,
        family: ProtocolFamily,
        clock: Arc<dyn ClockAuthority + Send + Sync>,
    ) -> Result<Arc<dyn CredentialOwner>, String> {
        source
            .validate_source()
            .map_err(|error| error.to_string())?;
        // Establish the revision and the configured account before opening sign-in.
        let initial = source.current().map_err(|error| error.to_string())?;
        let sign_in = if family == ProtocolFamily::ChatGptCodex {
            match source.chatgpt_sign_in() {
                Some((profile, files)) => {
                    let session = Session::open_with_files(
                        profile,
                        initial.credential.account().as_str(),
                        files.clone(),
                    )
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| chatgpt::SignInError::Unfinished.to_string())?;
                    let session = match source.sign_in_lock_authority() {
                        Some(authority) => session.with_lock_authority(authority),
                        None => session,
                    };
                    Some(SignIn {
                        session,
                        client: chatgpt::production_client().map_err(|error| error.to_string())?,
                        issuer: chatgpt::ISSUER.to_owned(),
                    })
                }
                None => None,
            }
        } else {
            None
        };
        let cloud = match family {
            ProtocolFamily::VertexGemini | ProtocolFamily::VertexAnthropic
                if !source.is_vertex_adc() =>
            {
                Some(CloudSession::Vertex(
                    source.vertex_session().map_err(|e| e.to_string())?,
                ))
            }
            ProtocolFamily::WatsonxChat => Some(CloudSession::Watsonx(
                source.watsonx_session().map_err(|e| e.to_string())?,
            )),
            _ => None,
        };
        Ok(Arc::new(Self {
            state: Arc::new(Mutex::new(Some(State {
                source,
                sign_in,
                cloud,
                unaccepted_refresh: None,
                accepted_revision: None,
            }))),
            gate: Arc::new(tokio::sync::Semaphore::new(1)),
            clock,
        }))
    }
}

async fn capture_recovery(
    state: &mut State,
    clock: &(dyn ClockAuthority + Sync),
    allow_refresh: bool,
) -> Result<CurrentCredential, CredentialRenewalRefusal> {
    if let Some(cloud) = &mut state.cloud {
        cloud.current(&mut state.source)?;
        if allow_refresh {
            cloud.prepare(clock, true, true).await?;
        }
        return cloud.current(&mut state.source);
    }
    let State {
        source, sign_in, ..
    } = state;
    match sign_in {
        Some(sign_in) => sign_in
            .session
            .recover_with(
                &sign_in.client,
                &sign_in.issuer,
                clock,
                allow_refresh,
                || source.current(),
            )
            .await
            .map_err(sign_in_error)?
            .map_err(source_error),
        None => source.current().map_err(source_error),
    }
}

impl CredentialOwner for LiveCredentials {
    fn current(&self) -> CredentialRenewalFuture<'_, CurrentCredential> {
        Box::pin(async move {
            let mut custody = self.acquire_state().await?;
            let clock = self.clock.clone();
            tokio::spawn(async move {
                let _protection = grapt_login::profile_security::ProtectionBatch::begin()
                    .map_err(|_| CredentialRenewalRefusal::Unavailable)?;
                let state = custody
                    .value
                    .as_mut()
                    .ok_or(CredentialRenewalRefusal::Unavailable)?;
                state.source.validate_dispatch().map_err(source_error)?;
                let allow_refresh = state.unaccepted_refresh.is_none();
                let State {
                    source,
                    sign_in,
                    cloud,
                    ..
                } = &mut *state;
                let (current, refreshed) = if let Some(cloud) = cloud {
                    cloud.current(source)?;
                    let refreshed = cloud.prepare(clock.as_ref(), allow_refresh, false).await?;
                    (cloud.current(source)?, refreshed)
                } else {
                    match sign_in {
                        Some(sign_in) => {
                            let (renewal, current) = sign_in
                                .session
                                .prepare_with(
                                    &sign_in.client,
                                    &sign_in.issuer,
                                    clock.as_ref(),
                                    allow_refresh,
                                    |renewal| (renewal, source.current()),
                                )
                                .await
                                .map_err(sign_in_error)?;
                            (
                                current.map_err(source_error)?,
                                matches!(renewal, chatgpt::Renewal::Refreshed),
                            )
                        }
                        None => (source.current().map_err(source_error)?, false),
                    }
                };
                if refreshed || state.unaccepted_refresh.is_some() {
                    state.unaccepted_refresh = Some(current.revision);
                }
                Ok(current)
            })
            .await
            .map_err(|_| CredentialRenewalRefusal::Unavailable)?
        })
    }

    fn recover(&self, rejected: CredentialRevision) -> CredentialRenewalFuture<'_, bool> {
        Box::pin(async move {
            let mut custody = self.acquire_state().await?;
            let clock = self.clock.clone();
            tokio::spawn(async move {
                let _protection = grapt_login::profile_security::ProtectionBatch::begin()
                    .map_err(|_| CredentialRenewalRefusal::Unavailable)?;
                let state = custody
                    .value
                    .as_mut()
                    .ok_or(CredentialRenewalRefusal::Unavailable)?;
                state.source.validate_dispatch().map_err(source_error)?;
                let current = capture_recovery(state, clock.as_ref(), false).await?;
                if current.revision != rejected {
                    if state.accepted_revision != Some(current.revision) {
                        state.unaccepted_refresh = Some(current.revision);
                    }
                    return Ok(true);
                }
                if state.unaccepted_refresh == Some(rejected)
                    || (state.sign_in.is_none() && state.cloud.is_none())
                {
                    return Ok(false);
                }
                let replacement = capture_recovery(state, clock.as_ref(), true).await?;
                let changed = replacement.revision != rejected;
                state.unaccepted_refresh = Some(replacement.revision);
                Ok(changed)
            })
            .await
            .map_err(|_| CredentialRenewalRefusal::Unavailable)?
        })
    }

    fn accepted(&self, revision: CredentialRevision) -> CredentialRenewalFuture<'_, ()> {
        Box::pin(async move {
            let mut custody = self.acquire_state().await?;
            let state = custody
                .value
                .as_mut()
                .ok_or(CredentialRenewalRefusal::Unavailable)?;
            if state
                .accepted_revision
                .is_none_or(|accepted| revision > accepted)
            {
                state.accepted_revision = Some(revision);
            }
            if state.unaccepted_refresh == Some(revision) {
                state.unaccepted_refresh = None;
            }
            Ok(())
        })
    }
}
