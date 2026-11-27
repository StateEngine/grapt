// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: bounded direct reqwest transport, explicit deadlines,
// no POST retries or method-changing redirects, no executor/SDK dependency.
// Redirect checks.

pub mod auth;
mod headers;
mod response;
#[cfg(test)]
mod tests;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE, LOCATION};
use reqwest::{Method, StatusCode, Url};
use serde_json::Value;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use response::Incoming;

const MAX_REDIRECTS: usize = 10;
const MAX_URL_BYTES: usize = 8192;
const MAX_SESSION_BYTES: usize = 1024;
const MAX_RESPONSE_HEADERS: usize = 64;
const MAX_HEADER_BYTES: usize = 16 * 1024;
const SESSION: &str = "mcp-session-id";
const VERSION: &str = "mcp-protocol-version";

/// Messages omit endpoint URLs, server bodies, and header values deliberately.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("MCP endpoint must be HTTPS or an exact loopback HTTP address")]
    Endpoint,
    #[error("MCP transport limit must be between 1 byte and 1 MiB")]
    Limit,
    #[error("MCP TLS/client initialization failed")]
    Client,
    #[error("MCP HTTP exchange exceeded its deadline")]
    TimedOut,
    #[error("MCP HTTP transport failed; request outcome may be unknown")]
    Transport,
    #[error("MCP HTTP response status {0}")]
    Status(u16),
    #[error("MCP HTTP session expired; do not replay the pending request")]
    SessionExpired,
    #[error("MCP HTTP redirect is unsupported or leaves the configured origin")]
    Redirect,
    #[error("MCP HTTP headers are malformed, oversized, or change session identity")]
    Headers,
    #[error("MCP HTTP payload exceeds its bound")]
    Oversized,
    #[error("MCP HTTP payload is not a valid protocol message")]
    Protocol,
    #[error("MCP HTTP content type is unsupported")]
    ContentType,
    #[error("MCP authentication changed or requires sign-in; reconnect the server")]
    Authentication,
    #[error("MCP HTTP session state is unavailable")]
    State,
    #[error("MCP transport needs a Tokio runtime and positive budgets within 24 hours")]
    RuntimeOrBudget,
    #[error(transparent)]
    Sse(#[from] crate::SseDecodeError),
}

#[derive(Default)]
struct SessionHeaders {
    id: Option<HeaderValue>,
    protocol: Option<HeaderValue>,
    initializing: Option<Value>,
    generation: Option<grapt_types::McpLoginId>,
    authentication_bound: bool,
}

/// A remote endpoint's transport metadata, separate from MCP session authority.
#[derive(Clone)]
pub struct HttpClient {
    client: reqwest::Client,
    endpoint: Url,
    limit: usize,
    session: Arc<Mutex<SessionHeaders>>,
    configured_headers: HeaderMap,
    bearer: Option<Arc<dyn auth::BearerProvider>>,
    pub(crate) human_wait: crate::HumanWait,
}

impl fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpClient")
            .field("origin", &self.endpoint.origin().ascii_serialization())
            .field("max_frame_bytes", &self.limit)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum HttpReply {
    Accepted,
    Messages(Box<Incoming>),
}

impl HttpClient {
    pub(crate) fn frame_limit(&self) -> usize {
        self.limit
    }
    /// HTTP is accepted only for literal loopback addresses; a domain name
    /// cannot acquire local-network access by changing its DNS answer.
    pub fn new(endpoint: &str, max_frame_bytes: usize) -> Result<Self, HttpError> {
        Self::new_with_headers(
            endpoint,
            max_frame_bytes,
            &std::collections::BTreeMap::new(),
        )
    }

    pub fn new_with_headers(
        endpoint: &str,
        max_frame_bytes: usize,
        fields: &std::collections::BTreeMap<String, String>,
    ) -> Result<Self, HttpError> {
        let configured_headers = headers::configured(fields)?;
        if !(1..=1024 * 1024).contains(&max_frame_bytes) {
            return Err(HttpError::Limit);
        }
        if endpoint.len() > MAX_URL_BYTES
            || endpoint.chars().any(char::is_control)
            || endpoint.trim() != endpoint
        {
            return Err(HttpError::Endpoint);
        }
        let endpoint = Url::parse(endpoint).map_err(|_| HttpError::Endpoint)?;
        let loopback = endpoint
            .host_str()
            .and_then(|host| {
                host.trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .ok()
            })
            .is_some_and(|address| address.is_loopback());
        if endpoint.host().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
            || !(endpoint.scheme() == "https" || (endpoint.scheme() == "http" && loopback))
        {
            return Err(HttpError::Endpoint);
        }
        use rustls_platform_verifier::BuilderVerifierExt;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|_| HttpError::Client)?
            .with_platform_verifier()
            .map_err(|_| HttpError::Client)?
            .with_no_client_auth();
        let client = reqwest::Client::builder()
            .use_preconfigured_tls(tls)
            .https_only(endpoint.scheme() == "https")
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .pool_max_idle_per_host(0)
            .http2_max_header_list_size(MAX_HEADER_BYTES as u32)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| HttpError::Client)?;
        Ok(Self {
            client,
            endpoint,
            limit: max_frame_bytes,
            session: Arc::new(Mutex::new(SessionHeaders::default())),
            configured_headers,
            bearer: None,
            human_wait: crate::HumanWait::default(),
        })
    }

    pub fn with_bearer(mut self, bearer: Arc<dyn auth::BearerProvider>) -> Result<Self, HttpError> {
        if self
            .configured_headers
            .contains_key(reqwest::header::AUTHORIZATION)
        {
            return Err(HttpError::Authentication);
        }
        self.bearer = Some(bearer);
        Ok(self)
    }

    fn bind_generation(&self, lease: Option<&auth::BearerLease>) -> Result<(), HttpError> {
        let mut session = self.session.lock().map_err(|_| HttpError::State)?;
        if lease.is_some_and(|lease| {
            lease.revision == 0
                || lease.generation.as_str().len() > 128
                || lease.token.expose().is_empty()
                || lease.token.expose().len() > 16384
                || !lease
                    .token
                    .expose()
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-._~+/=".contains(&b))
        }) {
            return Err(HttpError::Authentication);
        }
        let next = lease.map(|lease| lease.generation.clone());
        if session.authentication_bound && session.generation != next {
            return Err(HttpError::Authentication);
        }
        session.authentication_bound = true;
        session.generation = next;
        Ok(())
    }

    /// POSTs may repeat only for same-origin 307/308 or one explicit 401
    /// followed by a refresh in the same login generation. Network failures
    /// and SSE EOF never replay a POST.
    pub async fn post(&self, frame: &[u8], budget: Duration) -> Result<HttpReply, HttpError> {
        if budget.is_zero() {
            return Err(HttpError::TimedOut);
        }
        if frame.len() > self.limit {
            return Err(HttpError::Oversized);
        }
        let message: Value = serde_json::from_slice(frame).map_err(|_| HttpError::Protocol)?;
        if !message.is_object() || message["jsonrpc"] != "2.0" {
            return Err(HttpError::Protocol);
        }
        let initialization = message["method"] == "initialize";
        let request = message.get("id").is_some() && message.get("method").is_some();
        if initialization {
            self.session
                .lock()
                .map_err(|_| HttpError::State)?
                .initializing = Some(message["id"].clone());
        }
        let response = self
            .human_wait
            .deadline(budget)?
            .timeout(self.send(Method::POST, Some(frame), None))
            .await??;
        self.record_session(response.headers(), initialization)?;
        if response.status() == StatusCode::ACCEPTED {
            return if request {
                Err(HttpError::Protocol)
            } else {
                Ok(HttpReply::Accepted)
            };
        }
        if !request {
            return Err(HttpError::Protocol);
        }
        Ok(HttpReply::Messages(Box::new(Incoming::new(
            response,
            self.clone(),
        )?)))
    }

    /// Resume one broken SSE stream using its committed cursor. The caller
    /// applies ReconnectPolicy and its original operation deadline.
    pub async fn resume(&self, cursor: &str, budget: Duration) -> Result<Incoming, HttpError> {
        if budget.is_zero() {
            return Err(HttpError::TimedOut);
        }
        if cursor.is_empty() || cursor.len() > MAX_SESSION_BYTES {
            return Err(HttpError::Headers);
        }
        self.listen(Some(cursor), budget).await
    }

    pub(crate) async fn listen(
        &self,
        cursor: Option<&str>,
        budget: Duration,
    ) -> Result<Incoming, HttpError> {
        let cursor = cursor
            .map(|value| {
                if value.is_empty() || value.len() > MAX_SESSION_BYTES {
                    return Err(HttpError::Headers);
                }
                let mut header =
                    HeaderValue::from_bytes(value.as_bytes()).map_err(|_| HttpError::Headers)?;
                header.set_sensitive(true);
                Ok(header)
            })
            .transpose()?;
        let response = self
            .human_wait
            .deadline(budget)?
            .timeout(self.send(Method::GET, None, cursor))
            .await??;
        self.record_session(response.headers(), false)?;
        Incoming::new_sse(response, self.clone())
    }

    async fn send(
        &self,
        method: Method,
        frame: Option<&[u8]>,
        cursor: Option<HeaderValue>,
    ) -> Result<reqwest::Response, HttpError> {
        let lease = match &self.bearer {
            Some(provider) => provider.acquire(None).await?,
            None => None,
        };
        self.bind_generation(lease.as_ref())?;
        let result = self
            .send_authenticated(method.clone(), frame, cursor.clone(), lease.as_ref())
            .await;
        if !matches!(&result, Err(HttpError::Status(401))) {
            return result;
        }
        let (Some(provider), Some(previous)) = (&self.bearer, lease) else {
            return result;
        };
        let next = provider
            .acquire(Some(previous.revision))
            .await?
            .ok_or(HttpError::Authentication)?;
        self.bind_generation(Some(&next))?;
        self.send_authenticated(method, frame, cursor, Some(&next))
            .await
    }

    async fn send_authenticated(
        &self,
        method: Method,
        frame: Option<&[u8]>,
        cursor: Option<HeaderValue>,
        lease: Option<&auth::BearerLease>,
    ) -> Result<reqwest::Response, HttpError> {
        let mut target = self.endpoint.clone();
        for hop in 0..=MAX_REDIRECTS {
            let (session, protocol) = {
                let state = self.session.lock().map_err(|_| HttpError::State)?;
                (state.id.clone(), state.protocol.clone())
            };
            let mut request = self
                .client
                .request(method.clone(), target.clone())
                .headers(self.configured_headers.clone())
                .header(
                    ACCEPT,
                    if method == Method::POST {
                        "application/json, text/event-stream"
                    } else {
                        "text/event-stream"
                    },
                );
            if let Some(lease) = lease {
                let mut value = HeaderValue::from_str(&format!("Bearer {}", lease.token.expose()))
                    .map_err(|_| HttpError::Authentication)?;
                value.set_sensitive(true);
                request = request.header(reqwest::header::AUTHORIZATION, value);
            }
            if let Some(session) = session {
                request = request.header(SESSION, session);
            }
            if let Some(protocol) = protocol {
                request = request.header(VERSION, protocol);
            }
            if let Some(cursor) = &cursor {
                request = request.header("last-event-id", cursor);
            }
            if let Some(frame) = frame {
                request = request
                    .header(CONTENT_TYPE, "application/json")
                    .body(frame.to_vec());
            }
            let response = request.send().await.map_err(|_| HttpError::Transport)?;
            validate_headers(response.headers())?;
            let status = response.status();
            if status.is_redirection() {
                if !matches!(status.as_u16(), 307 | 308) || hop == MAX_REDIRECTS {
                    return Err(HttpError::Redirect);
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or(HttpError::Redirect)?;
                let next = target.join(location).map_err(|_| HttpError::Redirect)?;
                if next.origin() != self.endpoint.origin()
                    || !next.username().is_empty()
                    || next.password().is_some()
                    || next.as_str().len() > MAX_URL_BYTES
                {
                    return Err(HttpError::Redirect);
                }
                target = next;
                continue;
            }
            if status == StatusCode::NOT_FOUND && self.has_session()? {
                return Err(HttpError::SessionExpired);
            }
            if status != StatusCode::OK && status != StatusCode::ACCEPTED {
                return Err(HttpError::Status(status.as_u16()));
            }
            return Ok(response);
        }
        Err(HttpError::Redirect)
    }

    fn has_session(&self) -> Result<bool, HttpError> {
        Ok(self
            .session
            .lock()
            .map_err(|_| HttpError::State)?
            .id
            .is_some())
    }

    fn record_session(&self, headers: &HeaderMap, initialization: bool) -> Result<(), HttpError> {
        let Some(id) = headers.get(SESSION) else {
            return Ok(());
        };
        if headers.get_all(SESSION).iter().count() != 1
            || id.is_empty()
            || id.as_bytes().len() > MAX_SESSION_BYTES
            || !id
                .as_bytes()
                .iter()
                .all(|byte| (0x21..=0x7e).contains(byte))
        {
            return Err(HttpError::Headers);
        }
        let mut session = self.session.lock().map_err(|_| HttpError::State)?;
        if !initialization && session.id.as_ref() != Some(id) {
            return Err(HttpError::Headers);
        }
        let mut id = id.clone();
        id.set_sensitive(true);
        session.id = Some(id);
        Ok(())
    }

    fn observe_initialize(&self, frame: &[u8]) -> Result<(), HttpError> {
        let Some(id) = self
            .session
            .lock()
            .map_err(|_| HttpError::State)?
            .initializing
            .clone()
        else {
            return Ok(());
        };
        let message: Value = serde_json::from_slice(frame).map_err(|_| HttpError::Protocol)?;
        if message.get("id") != Some(&id) || message.get("result").is_none() {
            return Ok(());
        }
        let identity =
            grapt_mcp::accept_initialize(&message["result"]).map_err(|_| HttpError::Protocol)?;
        let mut session = self.session.lock().map_err(|_| HttpError::State)?;
        session.protocol = Some(HeaderValue::from_static(identity.negotiated_protocol()));
        session.initializing = None;
        Ok(())
    }
}

fn validate_headers(headers: &HeaderMap) -> Result<(), HttpError> {
    if headers.len() > MAX_RESPONSE_HEADERS {
        return Err(HttpError::Headers);
    }
    let mut bytes = 0usize;
    for (name, value) in headers {
        bytes += name.as_str().len() + value.as_bytes().len();
        if bytes > MAX_HEADER_BYTES {
            return Err(HttpError::Headers);
        }
    }
    Ok(())
}
