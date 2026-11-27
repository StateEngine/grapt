//! In-app ChatGPT sign-in attempts; network work owns no profile writer.
use super::*;
use grapt_login::chatgpt;
use grapt_types::{IdAuthority, SeatRecipientId};
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    MutexGuard,
};

mod challenge;
use challenge::Challenge;
mod attempts;
mod install;
use attempts::{Attempt, State};

const ATTEMPT_LIMIT: usize = 32;
const LIFETIME_MS: i64 = 15 * 60 * 1000;

#[derive(Default)]
pub(super) struct Runtime {
    attempts: Mutex<Vec<Arc<Attempt>>>,
    starting: AtomicBool,
}
fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    match value.lock() {
        Ok(value) => value,
        Err(poisoned) => poisoned.into_inner(),
    }
}
struct Starting(Arc<Runtime>);
impl Drop for Starting {
    fn drop(&mut self) {
        self.0.starting.store(false, Ordering::Release);
    }
}
struct Endpoints {
    issuer: String,
    backend: String,
    client: chatgpt::SignInClient,
}
impl Endpoints {
    fn production() -> Result<Self, String> {
        #[cfg(feature = "loopback-provider-testkit")]
        if let Some(raw) = std::env::var_os("GRAPT_CHATGPT_SIGN_IN_LOOPBACK") {
            let base = raw
                .into_string()
                .map_err(|_| "invalid sign-in fixture endpoint")?;
            grapt_provider::LoopbackBase::parse(&base)
                .map_err(|_| "invalid sign-in fixture endpoint")?;
            let client = chatgpt::loopback_client(&base)
                .map_err(|_| "sign-in fixture client unavailable")?;
            return Ok(Self {
                issuer: base.clone(),
                backend: base,
                client,
            });
        }
        Ok(Self {
            issuer: chatgpt::ISSUER.into(),
            backend: chatgpt::CODEX_BACKEND.into(),
            client: chatgpt::production_client().map_err(|_| "sign-in client unavailable")?,
        })
    }
}
struct PendingSignIn {
    before: Option<Connections>,
    label: String,
    target: Option<DtoString>,
    endpoints: Endpoints,
    challenge: Challenge,
}

impl ConnectionSetup {
    pub(super) fn connect_oauth_owned(
        &self,
        principal: SealedPrincipal,
        id: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        request: IntegrationOAuthConnectRequestDto,
        target: Option<DtoString>,
    ) -> ApplicationFuture<IntegrationOAuthConnectResponseDto, InvalidRequestErrorDto> {
        let owner = self.clone();
        Box::pin(async move {
            if id.as_str() != "openai"
                || !matches!(
                    request.method_id.as_str(),
                    "chatgpt-device" | "chatgpt-browser"
                )
            {
                return Err(invalid("this integration has no supported OAuth method"));
            }
            if !request.inputs.as_map().is_empty() {
                return Err(invalid("ChatGPT sign-in does not accept inputs"));
            }
            if owner
                .oauth
                .starting
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return Err(invalid("another sign-in is starting; retry shortly"));
            }
            let _starting = Starting(Arc::clone(&owner.oauth));
            let label = request
                .label
                .into_present()
                .map(|label| label.into_string())
                .unwrap_or_else(|| "ChatGPT".into());
            if label.is_empty() || label.len() > 512 {
                return Err(invalid("connection label must contain 1..=512 bytes"));
            }
            let location = owner.location(principal.clone(), query).await?;
            {
                let mut attempts = lock(&owner.oauth.attempts);
                attempts
                    .retain(|attempt| !attempt.expire(grapt_store::SystemClock.now().as_millis()));
                if attempts.len() >= ATTEMPT_LIMIT {
                    return Err(invalid(
                        "too many sign-in attempts; cancel an earlier attempt",
                    ));
                }
            }
            let permit = Arc::clone(&owner.operations)
                .try_acquire_owned()
                .map_err(|_| invalid("connections are being changed; retry shortly"))?;
            let root = owner.root.clone();
            let (before, permit) = tokio::task::spawn_blocking(move || {
                let before = Store::read_snapshot(&root)?;
                grapt_platform::profile_configuration::ConnectionChange::check_source(
                    &root,
                    before.as_ref(),
                )?;
                Ok::<_, std::io::Error>((before, permit))
            })
            .await
            .map_err(|_| invalid("connection owner stopped"))?
            .map_err(invalid)?;
            let endpoints = Endpoints::production().map_err(invalid)?;
            let challenge = Challenge::start(request.method_id.as_str(), &endpoints).await?;
            drop(permit);
            let created = grapt_store::SystemClock.now().as_millis();
            let expires = created
                .checked_add(LIFETIME_MS)
                .ok_or_else(|| invalid("sign-in expiry is unavailable"))?;
            let ids = grapt_store::OsRandomIds::seed()
                .map_err(|_| invalid("sign-in identity is unavailable"))?;
            let id = IntegrationAttemptIdDto::new(
                DtoString::try_new(format!("oauth_{}", ids.mint().as_str())).map_err(invalid)?,
            );
            let (url, instructions) = challenge.public();
            let public = json!({"location":location,"data":{
                "attemptID":id,"url":url,"instructions":instructions,
                "mode":"auto","time":{"created":created,"expires":expires}}});
            let public = decode(public).map_err(invalid)?;
            let mut attempt = Attempt::new(
                id,
                principal.principal().recipient().clone(),
                created,
                expires,
            );
            attempt.user_code = challenge.user_code().map(str::to_owned);
            let attempt = Arc::new(attempt);
            lock(&owner.oauth.attempts).push(Arc::clone(&attempt));
            let active = Arc::clone(&attempt);
            let task = tokio::spawn(async move {
                owner
                    .finish_oauth(
                        active,
                        PendingSignIn {
                            before,
                            label,
                            target,
                            endpoints,
                            challenge,
                        },
                    )
                    .await;
            });
            attempt.attach(task.abort_handle());
            Ok(public)
        })
    }

    async fn finish_oauth(&self, attempt: Arc<Attempt>, pending: PendingSignIn) {
        let PendingSignIn {
            before,
            label,
            target,
            endpoints,
            challenge,
        } = pending;
        let network = async {
            let tokens = challenge.tokens(&endpoints).await.map_err(|_| ())?;
            let listing = chatgpt::discover_models(&endpoints.client, &endpoints.backend, &tokens)
                .await
                .map_err(|_| ())?;
            Ok::<_, ()>((tokens, listing))
        };
        let outcome = tokio::time::timeout(
            std::time::Duration::from_millis(LIFETIME_MS as u64),
            network,
        )
        .await;
        let Ok(Ok((tokens, listing))) = outcome else {
            attempt.expire(grapt_store::SystemClock.now().as_millis());
            attempt.fail();
            return;
        };
        let Ok(permit) = Arc::clone(&self.operations).try_acquire_owned() else {
            attempt.fail();
            return;
        };
        if !attempt.begin_commit(grapt_store::SystemClock.now().as_millis()) {
            return;
        }
        let owner = self.clone();
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            owner.install_oauth(
                before,
                target,
                label,
                &endpoints.issuer,
                &endpoints.backend,
                tokens,
                &listing,
            )
        })
        .await;
        match result {
            Ok(Ok(())) => attempt.complete(),
            _ => attempt.fail(),
        }
    }

    pub(super) fn oauth_challenge_owned(
        &self,
        principal: SealedPrincipal,
        id: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        attempt: IntegrationAttemptIdDto,
    ) -> ApplicationFuture<IntegrationOAuthChallengeResponseDto, InvalidRequestErrorDto> {
        let owner = self.clone();
        Box::pin(async move {
            owner.location(principal.clone(), query).await?;
            let attempt = owner
                .oauth
                .find(&principal, &id, &attempt)
                .map_err(invalid)?;
            attempt.expire(grapt_store::SystemClock.now().as_millis());
            let user_code = if matches!(*lock(&attempt.state), State::Pending | State::Finishing) {
                attempt.user_code.as_deref()
            } else {
                None
            };
            decode(json!({"userCode":user_code})).map_err(invalid)
        })
    }

    pub(super) fn oauth_status_owned(
        &self,
        principal: SealedPrincipal,
        id: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        attempt: IntegrationAttemptIdDto,
    ) -> ApplicationFuture<IntegrationOAuthStatusResponseDto, InvalidRequestErrorDto> {
        let owner = self.clone();
        Box::pin(async move {
            let location = owner.location(principal.clone(), query).await?;
            let attempt = owner
                .oauth
                .find(&principal, &id, &attempt)
                .map_err(invalid)?;
            attempt.expire(grapt_store::SystemClock.now().as_millis());
            decode(json!({"location":location,"data":attempt.public()})).map_err(invalid)
        })
    }

    pub(super) fn complete_oauth_owned(
        &self,
        principal: SealedPrincipal,
        id: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        attempt: IntegrationAttemptIdDto,
        request: IntegrationOAuthCompleteRequestDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        let owner = self.clone();
        Box::pin(async move {
            owner.location(principal.clone(), query).await?;
            let attempt = owner
                .oauth
                .find(&principal, &id, &attempt)
                .map_err(invalid)?;
            attempt.expire(grapt_store::SystemClock.now().as_millis());
            if request.code.into_present().is_some() {
                return Err(invalid("this sign-in completes automatically"));
            }
            if !matches!(
                *lock(&attempt.state),
                State::Pending | State::Finishing | State::Complete
            ) {
                return Err(invalid("this sign-in attempt is no longer active"));
            }
            Ok(NoContent)
        })
    }

    pub(super) fn cancel_oauth_owned(
        &self,
        principal: SealedPrincipal,
        id: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        attempt: IntegrationAttemptIdDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        let owner = self.clone();
        Box::pin(async move {
            owner.location(principal.clone(), query).await?;
            if let Ok(attempt) = owner.oauth.find(&principal, &id, &attempt) {
                if attempt.cancel() {
                    lock(&owner.oauth.attempts).retain(|held| held.id != attempt.id);
                }
            }
            Ok(NoContent)
        })
    }
}
