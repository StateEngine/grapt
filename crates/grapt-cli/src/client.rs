//! The only HTTP transport used by the CLI.

pub mod chatgpt;
mod claude_plugin;
mod connections;
mod ephemeral;
mod events;
mod hook_import;
mod mcp;
mod prompt;
mod reasoning;
mod seat;
mod selection;
pub mod stall;
pub mod terminal;

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use grapt_api::u3a::ProjectListResponseDto;
use grapt_api::{
    InterruptRequestDto, PromptRequestDto, PromptResponseDto, PromptText, SessionActiveResponseDto,
    SessionCreateRequestDto, SessionMessagesResponseDto, SessionResponseDto, SessionsResponseDto,
    WireId,
};
use grapt_schema::{Optional, OptionalNullable, Required};
use grapt_types::SessionId;
use reqwest::{Method, StatusCode, Url};
use rustls_platform_verifier::BuilderVerifierExt;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::{AdvisoryError, AdvisoryEvent, CliLimits, ClientCredential, DiscoveredService};

pub(crate) const REQUEST_ALLOWANCE: Duration = Duration::from_secs(30);
/// The longest an open event stream may stay silent before the CLI treats it
/// as ended and rebinds. A stream has no total allowance: it lives as long as
/// the turn it reports.
pub(crate) const EVENT_IDLE_ALLOWANCE: Duration = Duration::from_secs(60);
const MAX_ERROR_BODY_BYTES: usize = 8 * 1_024;
const MAX_RESPONSE_BODY_BYTES: usize = 16_777_216;
const MAX_PENDING_ADVISORIES: usize = 64;
/// Consecutive frames of unknown kinds a stream may deliver before a
/// recognized one. Each refreshes the idle allowance, so without this bound a
/// stream of nothing this client knows would hold a turn forever.
const MAX_CONSECUTIVE_SKIPPED_FRAMES: u64 = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientMode {
    RemoteTls,
    AttributedLoopback,
    Embedded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientEndpoint {
    url: Url,
    mode: ClientMode,
}

impl ClientEndpoint {
    pub fn remote(value: &str) -> Result<Self, ApiClientError> {
        validate_endpoint(value, ClientMode::RemoteTls)
    }

    pub(crate) fn attributed_loopback(value: &str) -> Result<Self, ApiClientError> {
        validate_endpoint(value, ClientMode::AttributedLoopback)
    }

    #[cfg(feature = "loopback-testkit")]
    pub fn loopback_testkit(value: &str) -> Result<Self, ApiClientError> {
        validate_endpoint(value, ClientMode::AttributedLoopback)
    }

    pub fn mode(&self) -> ClientMode {
        self.mode
    }

    pub fn as_str(&self) -> &str {
        self.url.as_str()
    }

    fn route(&self, path: &str) -> Result<Url, ApiClientError> {
        self.url
            .join(path)
            .map_err(|_| ApiClientError::InvalidEndpoint)
    }
}

#[derive(Clone)]
pub struct ApiClient {
    endpoint: ClientEndpoint,
    client: reqwest::Client,
    event_client: reqwest::Client,
    basic: Option<Arc<Credential>>,
    limits: CliLimits,
    embedded: Option<Arc<EmbeddedChild>>,
    /// Consecutive transport failures on this endpoint, shared by the
    /// durable re-reads that poll it and by every clone of the client.
    streak: Arc<std::sync::Mutex<stall::TransportStreak>>,
}

struct EmbeddedChild {
    child: tokio::sync::Mutex<Option<tokio::process::Child>>,
}

impl core::fmt::Debug for ApiClient {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ApiClient")
            .field("endpoint", &self.endpoint)
            .field("authenticated", &self.basic.is_some())
            .field("embedded", &self.embedded.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct Credential {
    username: String,
    secret: String,
}

#[derive(Debug, Error)]
pub enum ApiClientError {
    #[error("endpoint is not an admitted absolute root URL")]
    InvalidEndpoint,
    #[error("TLS client construction failed: {0}")]
    Tls(String),
    #[error("HTTP client construction failed: {0}")]
    Build(String),
    #[error("request body encoding failed")]
    Encode,
    #[error("request transport failed before a complete response: {0:?}")]
    Transport(reqwest::Error),
    #[error(
        "transport to {service} stalled after {failures} consecutive transport failures; last: {last}"
    )]
    TransportStalled {
        failures: u32,
        service: String,
        last: String,
    },
    #[error("the embedded service exited with {status} before answering")]
    EmbeddedServiceExited { status: std::process::ExitStatus },
    #[error("server returned HTTP status {0}")]
    Status(StatusCode),
    #[error("the provider rejected this key")]
    ProviderKeyRejected,
    #[error("provider verification did not complete")]
    ProviderVerificationFailed,
    /// The service's own statement about an endpoint it could not connect.
    #[error("{0}")]
    EndpointRefused(String),
    #[error("no provider is connected to this profile yet; connect one with grapt login <provider>, then retry")]
    ProviderSetup,
    #[error("{}", grapt_api::u3a::INTEGRATION_GOOGLE_ADC_MESSAGE)]
    GoogleAdcCredentials,
    #[error("{}", grapt_api::u3a::INTEGRATION_AWS_CREDENTIALS_MESSAGE)]
    AwsCredentials,
    #[error("{}", grapt_api::u3a::INTEGRATION_AZURE_CREDENTIALS_MESSAGE)]
    AzureCredentials,
    #[error("{}", grapt_api::u3a::INTEGRATION_AWS_PROFILE_MESSAGE)]
    AwsProfileConfiguration,
    #[error("server returned HTTP status {status}: {detail}")]
    StatusDetail { status: StatusCode, detail: String },
    #[error("response body exceeded its finite {maximum}-byte bound")]
    ResponseTooLarge { maximum: usize },
    #[error("response was not the expected versioned DTO")]
    Decode,
    #[error("prompt text violates the API bound")]
    PromptBound,
    #[error("typed session identity could not be represented on the wire")]
    SessionWire,
    #[error("event stream did not return text/event-stream")]
    EventContentType,
    #[error("event stream ended before terminal durable truth")]
    EventClosed,
    #[error("event stream frame exceeded its finite {maximum}-byte bound")]
    EventFrameTooLarge { maximum: usize },
    #[error("event stream frame was refused: {0}")]
    EventFrame(#[from] AdvisoryError),
    #[error("event stream exceeded its finite {maximum}-event decoded queue bound")]
    EventPendingBound { maximum: usize },
    #[error("spawned child has no process identity")]
    MissingChildPid,
    #[error("spawned child pid {expected} does not own discovered service pid {found}")]
    EmbeddedOwnerMismatch { expected: u32, found: u32 },
}

#[derive(Debug)]
pub struct AdvisoryStream {
    response: reqwest::Response,
    buffered: Vec<u8>,
    pending: VecDeque<AdvisoryEvent>,
    limits: CliLimits,
    /// Well-formed frames of a kind this client does not know, skipped.
    skipped_kinds: u64,
    /// Of those, how many arrived since the last recognized frame.
    consecutive_skips: u64,
}

impl ApiClient {
    pub fn remote(
        endpoint: ClientEndpoint,
        credential: &ClientCredential,
        limits: CliLimits,
    ) -> Result<Self, ApiClientError> {
        if endpoint.mode != ClientMode::RemoteTls {
            return Err(ApiClientError::InvalidEndpoint);
        }
        Ok(Self {
            endpoint,
            client: production_client(true, Allowance::Request(REQUEST_ALLOWANCE))?,
            event_client: production_client(true, Allowance::EventStream(EVENT_IDLE_ALLOWANCE))?,
            basic: Some(Arc::new(credential_from(credential))),
            limits,
            embedded: None,
            streak: Arc::new(std::sync::Mutex::new(stall::TransportStreak::default())),
        })
    }

    pub fn from_discovered(
        discovered: &DiscoveredService,
        limits: CliLimits,
    ) -> Result<Self, ApiClientError> {
        let endpoint = ClientEndpoint::attributed_loopback(discovered.endpoint())?;
        let credential = discovered.credential();
        Ok(Self {
            endpoint,
            client: production_client(false, Allowance::Request(REQUEST_ALLOWANCE))?,
            event_client: production_client(false, Allowance::EventStream(EVENT_IDLE_ALLOWANCE))?,
            basic: Some(Arc::new(credential_from(credential))),
            limits,
            embedded: None,
            streak: Arc::new(std::sync::Mutex::new(stall::TransportStreak::default())),
        })
    }

    /// Build the embedded client, handing the child back on every refusal so
    /// the caller can kill and reap it rather than rely on `kill_on_drop`.
    #[allow(clippy::result_large_err)] // the child is returned unboxed for its cleanup
    pub fn from_embedded(
        discovered: &DiscoveredService,
        child: tokio::process::Child,
        limits: CliLimits,
    ) -> Result<Self, (ApiClientError, tokio::process::Child)> {
        let expected = match child.id() {
            Some(pid) => pid,
            None => return Err((ApiClientError::MissingChildPid, child)),
        };
        let found = discovered.registration().owner().pid().value();
        if expected != found {
            return Err((
                ApiClientError::EmbeddedOwnerMismatch { expected, found },
                child,
            ));
        }
        let mut client = match Self::from_discovered(discovered, limits) {
            Ok(client) => client,
            Err(error) => return Err((error, child)),
        };
        client.endpoint.mode = ClientMode::Embedded;
        client.embedded = Some(Arc::new(EmbeddedChild {
            child: tokio::sync::Mutex::new(Some(child)),
        }));
        Ok(client)
    }

    /// Transfer the one owned child for explicit shutdown. Its stdin pipe
    /// stays open until shutdown starts, including across client clones.
    /// Whether this client owns the service it talks to: a child this
    /// command started, which inherited this command's environment.
    pub(crate) fn owns_service(&self) -> bool {
        self.embedded.is_some()
    }

    pub(crate) async fn take_embedded_child(&self) -> Option<tokio::process::Child> {
        let embedded = self.embedded.as_ref()?;
        embedded.child.lock().await.take()
    }

    #[cfg(feature = "loopback-testkit")]
    pub fn loopback_testkit(
        endpoint: ClientEndpoint,
        limits: CliLimits,
    ) -> Result<Self, ApiClientError> {
        if endpoint.mode != ClientMode::AttributedLoopback {
            return Err(ApiClientError::InvalidEndpoint);
        }
        Ok(Self {
            endpoint,
            client: production_client(false, Allowance::Request(REQUEST_ALLOWANCE))?,
            event_client: production_client(false, Allowance::EventStream(EVENT_IDLE_ALLOWANCE))?,
            basic: None,
            limits,
            embedded: None,
            streak: Arc::new(std::sync::Mutex::new(stall::TransportStreak::default())),
        })
    }

    /// The loopback test seam with explicit allowances: the stall witnesses
    /// drive one request allowance of a few seconds, not 30.
    #[cfg(feature = "loopback-testkit")]
    pub fn loopback_testkit_allowance(
        endpoint: ClientEndpoint,
        limits: CliLimits,
        request: Duration,
        idle: Duration,
    ) -> Result<Self, ApiClientError> {
        if endpoint.mode != ClientMode::AttributedLoopback {
            return Err(ApiClientError::InvalidEndpoint);
        }
        Ok(Self {
            endpoint,
            client: production_client(false, Allowance::Request(request))?,
            event_client: production_client(false, Allowance::EventStream(idle))?,
            basic: None,
            limits,
            embedded: None,
            streak: Arc::new(std::sync::Mutex::new(stall::TransportStreak::default())),
        })
    }

    /// The same client without a share in an embedded child's lifetime, for
    /// work that can outlive its caller, such as a run blocked in a write. The
    /// child stays owned by `self` and ends when `self` is dropped.
    pub(crate) fn without_child_ownership(&self) -> Self {
        Self {
            embedded: None,
            ..self.clone()
        }
    }

    pub fn endpoint(&self) -> &ClientEndpoint {
        &self.endpoint
    }

    pub const fn limits(&self) -> CliLimits {
        self.limits
    }

    pub async fn sessions(&self) -> Result<SessionsResponseDto, ApiClientError> {
        self.send_json::<(), SessionsResponseDto>(Method::GET, "api/session", None)
            .await
    }

    /// Newest-first user Sessions: subagent children are excluded, matching
    /// the oracle's continue selection (`parentID: null`, `order: desc`).
    pub async fn top_level_sessions(&self) -> Result<SessionsResponseDto, ApiClientError> {
        self.send_json::<(), SessionsResponseDto>(
            Method::GET,
            "api/session?parentID=null&order=desc",
            None,
        )
        .await
    }

    /// Continue the effective root-session query carried by the opaque cursor.
    pub(crate) async fn top_level_sessions_at(
        &self,
        cursor: &str,
    ) -> Result<SessionsResponseDto, ApiClientError> {
        self.get_json_query("api/session", &[("cursor", cursor)])
            .await
    }

    pub async fn session_active(&self, session: &SessionId) -> Result<bool, ApiClientError> {
        let response = self
            .send_json_resilient::<(), SessionActiveResponseDto>(
                Method::GET,
                "api/session/active",
                None,
            )
            .await?;
        Ok(response
            .data()
            .get()
            .as_map()
            .keys()
            .any(|candidate| candidate.as_inner() == session))
    }

    pub async fn session_permissions(
        &self,
        session: &SessionId,
    ) -> Result<grapt_api::PermissionListResponseDto, ApiClientError> {
        self.send_json_resilient::<(), grapt_api::PermissionListResponseDto>(
            Method::GET,
            &format!("api/session/{session}/permission"),
            None,
        )
        .await
    }

    pub async fn session(&self, session: &SessionId) -> Result<SessionResponseDto, ApiClientError> {
        self.send_json::<(), SessionResponseDto>(
            Method::GET,
            &format!("api/session/{session}"),
            None,
        )
        .await
    }

    pub async fn messages(
        &self,
        session: &SessionId,
    ) -> Result<SessionMessagesResponseDto, ApiClientError> {
        self.send_json_resilient::<(), SessionMessagesResponseDto>(
            Method::GET,
            &format!("api/session/{session}/message"),
            None,
        )
        .await
    }

    /// The history page a `cursor` of an earlier page points at. The cursor
    /// carries its own order, so none is sent with it. A cursor is URL-safe
    /// base64; anything else is not a cursor this API issued.
    pub async fn messages_at(
        &self,
        session: &SessionId,
        cursor: &str,
    ) -> Result<SessionMessagesResponseDto, ApiClientError> {
        let url_safe = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_');
        if cursor.is_empty() || !cursor.bytes().all(url_safe) {
            return Err(ApiClientError::Decode);
        }
        self.send_json_resilient::<(), SessionMessagesResponseDto>(
            Method::GET,
            &format!("api/session/{session}/message?cursor={cursor}"),
            None,
        )
        .await
    }

    /// The projects the server knows, each with its worktree.
    pub async fn projects(&self) -> Result<ProjectListResponseDto, ApiClientError> {
        self.send_json::<(), ProjectListResponseDto>(Method::GET, "api/project", None)
            .await
    }

    /// Forks `session` at its current end; the response is the new child Session.
    pub async fn fork_session(
        &self,
        session: &SessionId,
    ) -> Result<SessionResponseDto, ApiClientError> {
        self.send_json(
            Method::POST,
            &format!("api/session/{session}/fork"),
            Some(&serde_json::json!({})),
        )
        .await
    }

    pub async fn interrupt(&self, session: &SessionId) -> Result<(), ApiClientError> {
        let body = InterruptRequestDto::new(
            OptionalNullable::absent(),
            Optional::absent(),
            Optional::absent(),
        );
        self.send_no_content(
            Method::POST,
            &format!("api/session/{session}/interrupt"),
            &body,
        )
        .await
    }

    /// Renames the Session (its display title); the store's title rules
    /// apply server-side, so a refusal is the store's, not the wire's.
    pub async fn rename_session(
        &self,
        session: &SessionId,
        title: &str,
    ) -> Result<(), ApiClientError> {
        self.send_no_content(
            Method::POST,
            &format!("api/session/{session}/rename"),
            &serde_json::json!({"title": title}),
        )
        .await
    }

    /// Sets the Session's permission preset (its trust level).
    pub async fn select_preset(
        &self,
        session: &SessionId,
        preset: &str,
    ) -> Result<(), ApiClientError> {
        self.send_no_content(
            Method::POST,
            &format!("api/session/{session}/preset"),
            &serde_json::json!({"preset": preset}),
        )
        .await
    }

    /// Answers one pending permission request: `once` or `reject`.
    pub async fn reply_permission(
        &self,
        session: &SessionId,
        request: &str,
        reply: &str,
        message: Option<&str>,
    ) -> Result<(), ApiClientError> {
        let mut body = serde_json::json!({"reply": reply});
        if let Some(message) = message {
            body["message"] = Value::String(message.to_owned());
        }
        self.send_no_content(
            Method::POST,
            &format!("api/session/{session}/permission/{request}/reply"),
            &body,
        )
        .await
    }

    /// Reads one ordinary versioned API resource without introducing a second
    /// transport vocabulary in protocol adapters.
    pub(crate) async fn get_value(&self, path: &str) -> Result<Value, ApiClientError> {
        self.send_json::<(), Value>(Method::GET, path, None).await
    }

    pub(crate) async fn get_value_query(
        &self,
        path: &str,
        pairs: &[(&str, &str)],
    ) -> Result<Value, ApiClientError> {
        self.get_json_query(path, pairs).await
    }

    async fn get_json_query<R: DeserializeOwned>(
        &self,
        path: &str,
        pairs: &[(&str, &str)],
    ) -> Result<R, ApiClientError> {
        let mut url = self.endpoint.route(path)?;
        url.query_pairs_mut().extend_pairs(pairs.iter().copied());
        let mut request = self.client.request(Method::GET, url);
        if let Some(credential) = &self.basic {
            request = request.basic_auth(&credential.username, Some(&credential.secret));
        }
        let response = request.send().await.map_err(ApiClientError::Transport)?;
        let response = json_response(response).await?;
        let body = bounded_body(response, MAX_RESPONSE_BODY_BYTES).await?;
        serde_json::from_slice(&body).map_err(|_| ApiClientError::Decode)
    }

    /// Sends an already validated JSON body through the ordinary API client.
    pub(crate) async fn post_value(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<Value, ApiClientError> {
        self.send_json(Method::POST, path, Some(body)).await
    }

    pub(crate) async fn post_empty(&self, path: &str, body: &Value) -> Result<(), ApiClientError> {
        self.send_no_content(Method::POST, path, body).await
    }

    pub(crate) async fn put_empty_query(
        &self,
        path: &str,
        pairs: &[(&str, &str)],
        body: &Value,
    ) -> Result<(), ApiClientError> {
        let mut url = self.endpoint.route(path)?;
        url.query_pairs_mut().extend_pairs(pairs.iter().copied());
        let bytes = serde_json::to_vec(body).map_err(|_| ApiClientError::Encode)?;
        if bytes.len() > self.limits.values().stdin_bytes {
            return Err(ApiClientError::ResponseTooLarge {
                maximum: self.limits.values().stdin_bytes,
            });
        }
        let mut request = self
            .client
            .request(Method::PUT, url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(bytes);
        if let Some(credential) = &self.basic {
            request = request.basic_auth(&credential.username, Some(&credential.secret));
        }
        let response = request.send().await.map_err(ApiClientError::Transport)?;
        if !response.status().is_success() {
            return Err(ApiClientError::Status(response.status()));
        }
        let body = bounded_body(response, 1).await?;
        if body.is_empty() {
            Ok(())
        } else {
            Err(ApiClientError::Decode)
        }
    }

    pub(crate) async fn delete_empty(&self, path: &str) -> Result<(), ApiClientError> {
        let response = self
            .request(Method::DELETE, path)?
            .send()
            .await
            .map_err(ApiClientError::Transport)?;
        if !response.status().is_success() {
            return Err(ApiClientError::Status(response.status()));
        }
        let body = bounded_body(response, 1).await?;
        if body.is_empty() {
            Ok(())
        } else {
            Err(ApiClientError::Decode)
        }
    }

    pub fn wire_session(session: SessionId) -> Result<WireId<SessionId>, ApiClientError> {
        WireId::parse(session.to_string()).ok_or(ApiClientError::SessionWire)
    }

    async fn send_json<B, R>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<R, ApiClientError>
    where
        B: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let mut request = self.request(method, path)?;
        if let Some(body) = body {
            let bytes = serde_json::to_vec(body).map_err(|_| ApiClientError::Encode)?;
            if bytes.len() > self.limits.values().stdin_bytes {
                return Err(ApiClientError::ResponseTooLarge {
                    maximum: self.limits.values().stdin_bytes,
                });
            }
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(bytes);
        }
        let response = request.send().await.map_err(ApiClientError::Transport)?;
        let response = json_response(response).await?;
        let body = bounded_body(response, MAX_RESPONSE_BODY_BYTES).await?;
        serde_json::from_slice(&body).map_err(|_| ApiClientError::Decode)
    }

    /// The durable re-reads the run loop reissues after every advisory end
    /// and every idle tick: the same read is a re-read, never a new effect,
    /// so one transport failure may reissue it under the bounded streak in
    /// [`stall::TransportStreak`]. Admission, replies and prompts are never
    /// routed here, and a non-transport refusal empties the streak.
    async fn send_json_resilient<B, R>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<R, ApiClientError>
    where
        B: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        loop {
            // The budget is checked before every attempt: a re-read whose
            // full request allowance would cross it is never started, no
            // matter which of the shared re-reads asks for it.
            let gate = {
                let streak = self.streak();
                (!streak.may_reissue())
                    .then(|| (streak.failures(), streak.last_error().map(str::to_owned)))
            };
            if let Some((failures, last)) = gate {
                return Err(ApiClientError::TransportStalled {
                    failures,
                    service: self.endpoint.as_str().to_owned(),
                    last: last.unwrap_or_else(|| "no transport failure was recorded".to_owned()),
                });
            }
            let start = tokio::time::Instant::now();
            match self.send_json(method.clone(), path, body).await {
                Ok(response) => {
                    self.streak().reset();
                    return Ok(response);
                }
                Err(ApiClientError::Transport(transport)) => {
                    let waited = start.elapsed();
                    let last = transport.to_string();
                    let is_timeout = transport.is_timeout();
                    // A connect-kind failure may be the service dying under
                    // the embedded product — the child is the only proof of
                    // that, and a proven exit ends the run at once — and it
                    // buys the budget-clipped backoff. A timeout or a late
                    // body failure buys no backoff.
                    if transport.is_connect() && !is_timeout {
                        if let Some(status) = self.probe_embedded_exit().await {
                            return Err(ApiClientError::EmbeddedServiceExited { status });
                        }
                    }
                    let (backoff, stalled, failures, diagnostic) = {
                        let mut streak = self.streak();
                        let (backoff, stalled) = streak.record_failure(
                            waited,
                            is_timeout,
                            transport.is_connect(),
                            last.clone(),
                        );
                        (backoff, stalled, streak.failures(), streak.diagnostic_due())
                    };
                    if stalled {
                        return Err(ApiClientError::TransportStalled {
                            failures,
                            service: self.endpoint.as_str().to_owned(),
                            last,
                        });
                    }
                    if diagnostic {
                        eprintln!(
                            "grapt: the service at {} is not answering ({last}); retrying the durable reads — the run ends after {} consecutive transport failures or when the stall budget is spent",
                            self.endpoint.as_str(),
                            stall::MAX_CONSECUTIVE_TRANSPORT_FAILURES,
                        );
                    }
                    if !backoff.is_zero() {
                        tokio::time::sleep(backoff).await;
                    }
                }
                Err(other) => {
                    self.streak().reset();
                    return Err(other);
                }
            }
        }
    }

    fn streak(&self) -> std::sync::MutexGuard<'_, stall::TransportStreak> {
        self.streak
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The child is the only proof that an embedded service is gone: read
    /// it without waiting, and put it back when it is still running.
    async fn probe_embedded_exit(&self) -> Option<std::process::ExitStatus> {
        let embedded = self.embedded.as_ref()?;
        let mut slot = embedded.child.lock().await;
        let mut child = slot.take()?;
        match child.try_wait() {
            Ok(Some(status)) => Some(status),
            Ok(None) | Err(_) => {
                *slot = Some(child);
                None
            }
        }
    }

    async fn send_no_content<B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: &B,
    ) -> Result<(), ApiClientError> {
        let bytes = serde_json::to_vec(body).map_err(|_| ApiClientError::Encode)?;
        let response = self
            .request(method, path)?
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(bytes)
            .send()
            .await
            .map_err(ApiClientError::Transport)?;
        if !response.status().is_success() {
            return Err(ApiClientError::Status(response.status()));
        }
        let body = bounded_body(response, 1).await?;
        if body.is_empty() {
            Ok(())
        } else {
            Err(ApiClientError::Decode)
        }
    }

    fn request(
        &self,
        method: Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder, ApiClientError> {
        Ok(self.authorized(self.client.request(method, self.endpoint.route(path)?)))
    }

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.basic {
            Some(credential) => request.basic_auth(&credential.username, Some(&credential.secret)),
            None => request,
        }
    }
}

impl AdvisoryStream {
    /// How many well-formed frames of an unknown kind this stream skipped.
    pub fn skipped_kinds(&self) -> u64 {
        self.skipped_kinds
    }

    pub async fn next(&mut self) -> Result<AdvisoryEvent, ApiClientError> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Ok(event);
            }
            let chunk = self
                .response
                .chunk()
                .await
                .map_err(ApiClientError::Transport)?
                .ok_or(ApiClientError::EventClosed)?;
            self.ingest(&chunk)?;
        }
    }

    fn ingest(&mut self, chunk: &[u8]) -> Result<(), ApiClientError> {
        let maximum = self.limits.values().sse_frame_bytes;
        for byte in chunk {
            self.buffered.push(*byte);
            if self.buffered.len() > maximum {
                return Err(ApiClientError::EventFrameTooLarge { maximum });
            }
            let delimiter = if self.buffered.ends_with(b"\r\n\r\n") {
                Some(4)
            } else if self.buffered.ends_with(b"\n\n") {
                Some(2)
            } else {
                None
            };
            if let Some(delimiter) = delimiter {
                let frame_length = self.buffered.len().saturating_sub(delimiter);
                self.buffered.truncate(frame_length);
                let frame = core::mem::take(&mut self.buffered);
                // A kind this client does not know is a newer server's
                // vocabulary, not a broken stream: skip it, as the TUI does,
                // rather than end the turn. Every other refusal (envelope,
                // identity, a known kind at an unexpected version) is still
                // an error, because that is real protocol drift.
                // Past the consecutive bound the stream has stopped making
                // progress for this client: it ends, so ACP rebinds and reads
                // durable truth under its own quiet-rebind bound.
                let event = match AdvisoryEvent::decode_sse(&frame, self.limits) {
                    Err(AdvisoryError::EventKind) => {
                        self.skipped_kinds = self.skipped_kinds.saturating_add(1);
                        self.consecutive_skips = self.consecutive_skips.saturating_add(1);
                        if self.consecutive_skips >= MAX_CONSECUTIVE_SKIPPED_FRAMES {
                            return Err(ApiClientError::EventClosed);
                        }
                        continue;
                    }
                    decoded => decoded?,
                };
                self.consecutive_skips = 0;
                if self.pending.len() >= MAX_PENDING_ADVISORIES {
                    return Err(ApiClientError::EventPendingBound {
                        maximum: MAX_PENDING_ADVISORIES,
                    });
                }
                self.pending.push_back(event);
            }
        }
        Ok(())
    }
}

fn credential_from(credential: &ClientCredential) -> Credential {
    let (username, secret) = credential.parts();
    Credential {
        username: username.to_owned(),
        secret: secret.to_owned(),
    }
}

async fn bounded_body(
    mut response: reqwest::Response,
    maximum: usize,
) -> Result<Vec<u8>, ApiClientError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(ApiClientError::Transport)? {
        let next = bytes.len().saturating_add(chunk.len());
        if next > maximum {
            return Err(ApiClientError::ResponseTooLarge { maximum });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn json_response(response: reqwest::Response) -> Result<reqwest::Response, ApiClientError> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let detail = bounded_body(response, MAX_ERROR_BODY_BYTES)
        .await
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .filter(|value| !value.is_empty());
    Err(match detail {
        Some(detail) if provider_setup(status, &detail) => ApiClientError::ProviderSetup,
        Some(detail) => ApiClientError::StatusDetail { status, detail },
        None => ApiClientError::Status(status),
    })
}

/// A service with no connected provider answers a run as unavailable for
/// provider setup; that is a missing connection, not an outage.
fn provider_setup(status: StatusCode, detail: &str) -> bool {
    status == StatusCode::SERVICE_UNAVAILABLE
        && serde_json::from_str::<serde_json::Value>(detail).is_ok_and(|body| {
            body["_tag"] == "ServiceUnavailableError" && body["service"] == "provider-setup"
        })
}

fn validate_endpoint(value: &str, mode: ClientMode) -> Result<ClientEndpoint, ApiClientError> {
    let mut url = Url::parse(value).map_err(|_| ApiClientError::InvalidEndpoint)?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(ApiClientError::InvalidEndpoint);
    }
    match mode {
        ClientMode::RemoteTls if url.scheme() == "https" => {}
        ClientMode::AttributedLoopback if url.scheme() == "http" => {
            let host = url.host_str().ok_or(ApiClientError::InvalidEndpoint)?;
            let address = host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<IpAddr>()
                .map_err(|_| ApiClientError::InvalidEndpoint)?;
            if !address.is_loopback() {
                return Err(ApiClientError::InvalidEndpoint);
            }
        }
        _ => return Err(ApiClientError::InvalidEndpoint),
    }
    url.set_path("/");
    Ok(ClientEndpoint { url, mode })
}

/// How long one request may take. reqwest's total timeout covers the whole
/// response body, so a streamed body needs its own client.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Allowance {
    /// Connect and complete the response within the carried total.
    Request(Duration),
    /// Unbounded total; each read within the carried idle bound.
    EventStream(Duration),
}

pub(crate) fn production_client(
    https_only: bool,
    allowance: Allowance,
) -> Result<reqwest::Client, ApiClientError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| ApiClientError::Tls(error.to_string()))?;
    let tls = builder
        .with_platform_verifier()
        .map_err(|error| ApiClientError::Tls(error.to_string()))?
        .with_no_client_auth();
    let builder = reqwest::Client::builder().connect_timeout(REQUEST_ALLOWANCE);
    let builder = match allowance {
        Allowance::Request(total) => builder.timeout(total),
        Allowance::EventStream(idle) => builder.read_timeout(idle),
    };
    let builder = builder
        .pool_max_idle_per_host(0)
        .pool_idle_timeout(None)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .use_preconfigured_tls(tls);
    let builder = if https_only {
        builder.https_only(true)
    } else {
        builder
    };
    builder
        .build()
        .map_err(|error| ApiClientError::Build(error.to_string()))
}
