//! The real transport: authenticated HTTP/1.1 over a real socket.
//!
//! # What this closes
//!
//! A review blocker: the only [`crate::connection::Connection`] was the contract
//! double, the shipped binary constructed it, and the startup banner still said
//! the real seam "arrives with DOGFOOD-1" after DOGFOOD-1 had landed. The
//! generic type parameter was offered as the seam. It is not one — a type
//! parameter proves that a second implementation COULD exist, never that one
//! does, and every reconnect and restart law in this crate was being certified
//! against a path production does not take.
//!
//! This is that second implementation, and `tests/dogfood_journey.rs` drives it
//! against a real `grapt-server` process over a real socket.
//!
//! # Why it is hand-rolled
//!
//! Exactly one new crates.io name was admitted to this crate, `crossterm`,
//! and admitted it because the workspace forbids `unsafe_code` and terminal raw
//! mode cannot be reached without it. An HTTP client is not in that position:
//! HTTP/1.1 request framing and a `Content-Length` response are a few hundred
//! lines of `std::net` and `std::io`, and adding `reqwest` or `hyper` here would
//! expand the audited external surface of a CLIENT crate for convenience. So
//! this module speaks the protocol directly, and
//! `tests/allowed_external_dependencies.rs` still attests the same admitted set.
//!
//! It is also deliberately SYNCHRONOUS, matching [`crate::transport::Transport`]
//! and the sync/async constitution: sockets are touched here, and the domain
//! core — cursor, projection, view model — still contains no `.await` at all.
//!
//! # Byte-preserving, and what that actually means
//!
//! The bytes `crate::wire` encodes are the bytes written to the socket, and the
//! bytes read from the socket are the bytes `crate::wire` decodes. This module
//! parses no DTO, re-serializes nothing, and does not normalize the body. That
//! is the property that makes the double and the real server interchangeable
//! witnesses: if this module re-encoded anything, a body that round-tripped
//! here could differ from one the double served, and the two would stop proving
//! the same thing.
//!
//! # The response bound is PRE-allocation
//!
//! [`crate::transport::Transport`]'s contract says the body is bounded "BEFORE
//! unbounded allocation. An implementation that read the whole body and then
//! measured it would satisfy the signature and violate the law." So a declared
//! `Content-Length` over the bound is refused without reading the body at all,
//! and a body of undeclared length is read into a buffer that refuses at
//! `bound + 1` — enough to KNOW it overflowed, never enough to be exhausted by
//! it. The client's own ingress bound still applies afterwards; two bounds on
//! one fact is deliberate here, because only this one can act before the
//! allocation happens.
//!
//! # No ambient clock
//!
//! There is no `Instant::now` and no `SystemTime::now` in this module — both are
//! `disallowed_methods` workspace-wide. Every wait is bounded by a socket read
//! timeout the OS enforces, and every loop by an explicit iteration budget.

use std::fmt;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use thiserror::Error;

use crate::bounds::Bounds;
use crate::connection::Connection;
use crate::contract::{Method, Operation};
use crate::transport::{
    FramePoll, SseReader, Transport, TransportError, WireRequest, WireResponse,
};

/// The largest response HEAD this client will read, in bytes.
///
/// Mirrors the API host's own `max_request_head_bytes` of 16 KiB. A client that
/// accepted an unbounded head would be exhaustible by a peer that never sent
/// the terminating blank line, which is the cheapest denial there is.
const MAX_HEAD_BYTES: usize = 16 * 1024;

/// Read-loop iteration budget.
///
/// Paired with the socket read timeout rather than replacing it: the timeout
/// bounds ONE read, this bounds how many a single call may make, so a peer that
/// dribbles one byte per timeout window cannot hold a call open forever.
const MAX_READS: usize = 4_096;

mod cancellation;
mod connection;
pub use cancellation::StreamCancellation;
mod watched;
pub use watched::WatchedSessions;
#[cfg(test)]
mod first_run_tests;
#[cfg(test)]
pub(crate) mod flush_tests;
mod pending;
mod recovery;
mod resume;
mod startup;
#[cfg(test)]
mod stream_poll_tests;
#[cfg(test)]
mod tool_read_tests;
mod window;
#[cfg(test)]
mod window_lease_tests;

/// Total allowance for acquiring one HTTP response.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// How long one observed startup form may spend in ACTIVE acquisition before
/// it is refused. The human allowance beside it is separate and much longer.
pub const STARTUP_ACTIVE_ALLOWANCE: Duration = Duration::from_secs(30);

fn request_timeout(operation: Operation) -> Duration {
    match operation {
        // History may wait for durable state and render stored tool results.
        // Keep the same bounded allowance as the CLI rather than the shorter
        // control/health window, including incremental recovery and tool reads.
        Operation::MessageList | Operation::SessionSnapshot => Duration::from_secs(30),
        // Covers interrupt drain, cleanup, and pending-input resume.
        Operation::SessionInterrupt => Duration::from_secs(60),
        // Key setup performs bounded provider checks before confirmation.
        Operation::IntegrationKeyConnect => Duration::from_secs(90),
        _ => READ_TIMEOUT,
    }
}

/// Initial connection allowance for the advisory event subscription.
const STREAM_CONNECT_TIMEOUT: Duration = Duration::from_millis(50);

/// Why the real transport failed.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum HttpError {
    #[error("endpoint `{value}` is not an `http://host:port` address this client can reach")]
    Endpoint { value: String },
    #[error("a Basic username may not contain `:`, and neither field may contain control bytes")]
    Credential,
    #[error("the response head exceeded {MAX_HEAD_BYTES} bytes with no terminator")]
    HeadTooLarge,
    #[error("the response head was malformed: {reason}")]
    HeadMalformed { reason: String },
    #[error("an active startup allowance of zero would refuse every startup form")]
    InvalidStartupAllowance,
}

impl From<HttpError> for TransportError {
    fn from(error: HttpError) -> Self {
        TransportError::Io {
            reason: error.to_string(),
        }
    }
}

/// One HTTP Basic credential, pre-encoded and REDACTED in every rendering.
///
/// The encoded header value is built once at construction rather than per
/// request, but that is not why this is a type. It is a type because
/// [`Transport`] requires `Debug`, and a struct holding a secret in a plain
/// field gets that secret printed by every `{:?}` in a log line, a panic
/// message or an error render. `Debug` is implemented by hand below to print a
/// fixed placeholder, and `tests/dogfood_journey.rs` asserts the secret does not
/// appear in the rendering of a connection that holds it.
#[derive(Clone, PartialEq, Eq)]
pub struct BasicCredential {
    header: String,
}

impl BasicCredential {
    /// Encode one credential, refusing the shapes that would forge a header.
    ///
    /// A `:` in the username is refused because Basic separates the two fields
    /// with one, so a username containing it silently redefines where the
    /// secret starts. Control bytes are refused because CR or LF in either
    /// field is header injection: the encoded form is base64 so it cannot carry
    /// them, but refusing at construction says why rather than relying on the
    /// encoding to launder it.
    pub fn new(username: &str, secret: &str) -> Result<Self, HttpError> {
        if username.contains(':') {
            return Err(HttpError::Credential);
        }
        if username
            .bytes()
            .chain(secret.bytes())
            .any(|byte| byte < 0x20 || byte == 0x7f)
        {
            return Err(HttpError::Credential);
        }
        let raw = format!("{username}:{secret}");
        Ok(Self {
            header: format!("Basic {}", base64(raw.as_bytes())),
        })
    }

    fn header_value(&self) -> &str {
        &self.header
    }
}

impl fmt::Debug for BasicCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BasicCredential(<redacted>)")
    }
}

/// Standard base64 with padding.
///
/// Hand-written for the same reason the HTTP is: one small, auditable function
/// beats an added dependency in a client crate whose external surface is
/// attested by a gate. It is exercised against the RFC 4648 vectors in
/// `tests/dogfood_journey.rs`, including all three padding cases, because an
/// encoder that is wrong only at a length boundary produces a credential that
/// fails authentication for reasons no error message will explain.
fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = usize::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, usize::from);
        let b2 = chunk.get(2).copied().map_or(0, usize::from);
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(char::from(ALPHABET[(triple >> 18) & 0x3f]));
        out.push(char::from(ALPHABET[(triple >> 12) & 0x3f]));
        out.push(if chunk.len() > 1 {
            char::from(ALPHABET[(triple >> 6) & 0x3f])
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            char::from(ALPHABET[triple & 0x3f])
        } else {
            '='
        });
    }
    out
}

/// Resolve `http://host:port` to a socket address.
///
/// Deliberately narrow: it accepts the exact spelling the product's own
/// readiness receipt publishes in `observed_endpoint`, and nothing else. There
/// is no DNS resolution and no `https`, because this client attaches to a
/// locally-published endpoint whose address the product observed and wrote
/// down. Widening this is a reviewed change, not an accident of parsing.
pub fn endpoint_address(endpoint: &str) -> Result<SocketAddr, HttpError> {
    endpoint
        .strip_prefix("http://")
        .and_then(|rest| rest.parse::<SocketAddr>().ok())
        .ok_or_else(|| HttpError::Endpoint {
            value: endpoint.to_owned(),
        })
}

/// The real connection: one authenticated endpoint and its event subscription.
pub struct HttpConnection {
    address: SocketAddr,
    credential: BasicCredential,
    bounds: Bounds,
    /// How long one observed startup form may spend in ACTIVE acquisition
    /// before it is refused, while the human allowance is separate.
    ///
    /// Production is [`STARTUP_ACTIVE_ALLOWANCE`]; a witness sets it through
    /// [`HttpConnection::attach_with_startup_allowance`] so the behaviour can
    /// be observed without waiting out the production thirty seconds.
    startup_active_allowance: Duration,
    /// The live `/api/event` subscription, if one is open.
    ///
    /// `None` is an ordinary state, not a broken one. The contract calls live
    /// delivery advisory, so a client with no subscription is merely a client
    /// that will reconcile — which is the same path a dropped socket takes.
    stream: Option<TcpStream>,
    /// The Session this window is watching, named on every event subscription.
    ///
    /// The service decides "the last window is gone" from what its registry
    /// saw, never from a window's self-report, so a crashed window is still
    /// detected. Sent on the first subscribe and on every resubscribe, so a
    /// reconnect inside the grace keeps its attachment.
    watched: WatchedSessions,
    reader: SseReader,
    expected_product: Option<grapt_api::ProductHealth>,
    subscription_attempt: Option<recovery::SubscriptionAttempt>,
    pending_read: Option<pending::PendingHttp>,
    pending_catalog: Option<pending::PendingHttp>,
    pending_tools: Option<pending::PendingHttp>,
    pending_operator: Option<pending::PendingHttp>,
    identity_pending: bool,
    /// Whether subscribes omit `positions`, after a snapshot 404 (B3 S2c-1b).
    legacy: recovery::LegacyLatch,
    /// A subscribe refusal the next `poll_frame` reports, so it is never
    /// mistaken for "not subscribed".
    refusal: Option<TransportError>,
    /// The client's offer to resume, sent only under its own binding.
    resume: Option<crate::connection::ResumeToken>,
    /// The binding the open stream was installed under.
    stream_binding: Option<crate::connection::StreamBinding>,
}

impl fmt::Debug for HttpConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The credential redacts itself, but the whole struct is rendered by
        // hand anyway so that adding a field cannot silently start printing one.
        formatter
            .debug_struct("HttpConnection")
            .field("address", &self.address)
            .field("credential", &self.credential)
            .field("subscribed", &self.stream.is_some())
            .finish()
    }
}

impl HttpConnection {
    /// Wait for a complete event on a dedicated subscription reader.
    /// Commands and terminal input must have separate owners while this blocks.
    /// Restore nonblocking public polls before returning to the caller.
    pub fn wait_frame(&mut self) -> Result<FramePoll, TransportError> {
        if let Some(frame) = self.buffered_frame()? {
            return Ok(frame);
        }
        let Some(stream) = self.stream.as_ref() else {
            return Ok(FramePoll::Closed);
        };
        // resubscribe and incremental recovery both install nonblocking
        // sockets. Clearing their timeout alone cannot make a read wait.
        if stream
            .set_nonblocking(false)
            .and_then(|()| stream.set_read_timeout(None))
            .is_err()
        {
            self.closed()?;
            return Err(TransportError::NotConnected);
        }
        let result = loop {
            match self.poll_frame() {
                Ok(FramePoll::Idle) => continue,
                result => break result,
            }
        };
        if self
            .stream
            .as_ref()
            .is_some_and(|stream| stream.set_nonblocking(true).is_err())
        {
            self.closed()?;
            return Err(TransportError::NotConnected);
        }
        result
    }

    /// Authenticate and verify the versioned identity supplied by the launch adapter.
    pub fn verify_product(
        &self,
        expected: &grapt_api::ProductHealth,
    ) -> Result<(), TransportError> {
        let response = self.exchange(
            Method::Get,
            grapt_api::PRODUCT_HEALTH_PATH,
            &[],
            None,
            4_096,
            READ_TIMEOUT,
        )?;
        Self::validate_product(expected, response)
    }

    /// Recheck the pinned product through a bounded health RPC.
    /// This performs blocking I/O and belongs on a command worker.
    /// Connections without a product pin have no identity to verify.
    pub fn verify_pinned_product(&self) -> Result<(), TransportError> {
        match &self.expected_product {
            Some(expected) => self.verify_product(expected),
            None => Ok(()),
        }
    }

    fn validate_product(
        expected: &grapt_api::ProductHealth,
        response: WireResponse,
    ) -> Result<(), TransportError> {
        if response.status != 200 {
            return Err(TransportError::Io { reason: format!("product authentication/health returned {}; repair the profile or restart the product", response.status) });
        }
        let observed: grapt_api::ProductHealth =
            serde_json::from_slice(&response.body).map_err(|_| TransportError::Io {
                reason: "product health DTO is invalid".into(),
            })?;
        if &observed != expected {
            return Err(TransportError::Io {
                reason: "endpoint belongs to a different product; restart the terminal".into(),
            });
        }
        Ok(())
    }

    /// Keep advisory stream recovery while refusing loss/replacement of the
    /// product this local terminal actually attached to.
    pub fn pin_product(&mut self, health: grapt_api::ProductHealth) {
        self.expected_product = Some(health);
    }

    fn closed(&mut self) -> Result<FramePoll, TransportError> {
        self.stream = None;
        // Losing an installed legacy stream ends the latch: the next
        // subscribe probes positions again.
        self.legacy.stream_lost();
        self.identity_pending = self.expected_product.is_some();
        // The reconnect attempt verifies the pinned product before opening a
        // new subscription. A slow health response must not block key input.
        Ok(FramePoll::Closed)
    }

    fn push_stream_bytes(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        match self.reader.push_bytes(bytes) {
            Ok(_) | Err(TransportError::FrameTooLarge { .. }) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn buffered_frame(&mut self) -> Result<Option<FramePoll>, TransportError> {
        if self.reader.overflowed() {
            // No suffix of a refused frame may be interpreted as another event.
            self.closed()?;
            self.reader = SseReader::new(self.bounds);
            return Ok(Some(FramePoll::Gap));
        }
        Ok(self.reader.take_frame())
    }

    fn require_product_identity(&self) -> Result<(), TransportError> {
        if self.identity_pending {
            return Err(TransportError::Unavailable {
                reason: "checking the connected product; retry shortly".into(),
            });
        }
        Ok(())
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// Independent RPC ownership without copying streams or pending requests.
    /// The caller must bound how many command peers it retains.
    pub fn command_peer(&self) -> Result<Self, TransportError> {
        self.require_product_identity()?;
        let mut peer = Self::attach(self.address, self.credential.clone(), self.bounds);
        peer.expected_product = self.expected_product.clone();
        Ok(peer)
    }

    /// Whether a live subscription is currently open.
    pub fn subscribed(&self) -> bool {
        self.stream.is_some()
    }

    /// The Session this window currently names on its event subscription.
    pub(super) fn watched_session(&self) -> Option<&grapt_types::SessionId> {
        self.watched.current()
    }

    /// The Sessions a subscription names besides the one shown.
    pub(super) fn retained_sessions(&self) -> &[grapt_types::SessionId] {
        self.watched.retained()
    }

    /// Name the Session this window is watching.
    ///
    /// The next subscription carries it, and so does every resubscribe after
    /// it. The Session being left stays claimed as retained, so switching
    /// Sessions or opening a subagent's view never leaves the previous
    /// Session's turn to the close grace.
    pub fn watch_session(&mut self, session: Option<grapt_types::SessionId>) {
        if self.watched.show(session) {
            self.forget_attachment();
        }
    }

    /// Adopt a window's whole watched state, as a replacement connection does,
    /// so the Sessions it left stay claimed across a reconnect.
    pub fn adopt_watched(&mut self, watched: WatchedSessions) {
        if self.watched != watched {
            self.watched = watched;
            self.forget_attachment();
        }
    }

    /// Drop the attachment built for the Sessions watched before.
    fn forget_attachment(&mut self) {
        // The old attachment describes the previous Session. Drop the socket so
        // the next subscribe registers the new one rather than leaving a stale
        // window on the service.
        self.stream = None;
        // A subscription already in flight was built for the Session being left,
        // so it must not complete and register a lease the caller no longer
        // holds. Dropping the attempt fences it: the next reconnect builds its
        // request from the Session watched now.
        self.subscription_attempt = None;
    }

    /// Open one socket with both timeouts set.
    fn connect(&self, timeout: Duration) -> Result<TcpStream, TransportError> {
        let stream = TcpStream::connect_timeout(&self.address, timeout).map_err(|error| {
            TransportError::Io {
                reason: format!("connect {}: {error}", self.address),
            }
        })?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|error| TransportError::Io {
                reason: format!("read timeout: {error}"),
            })?;
        stream
            .set_write_timeout(Some(READ_TIMEOUT))
            .map_err(|error| TransportError::Io {
                reason: format!("write timeout: {error}"),
            })?;
        Ok(stream)
    }

    /// Write one request head and its body, verbatim.
    /// Percent-encode one query value.
    ///
    /// Everything outside the unreserved set of RFC 3986 is escaped, which is
    /// wider than strictly necessary and deliberately so: a cursor is an opaque
    /// server token and a location is a Windows path with backslashes, colons
    /// and spaces in it. Encoding only what "looks dangerous" is how a `&` in a
    /// directory name becomes a second query parameter.
    fn encode_query_value(value: &str) -> String {
        let mut out = String::with_capacity(value.len());
        for byte in value.as_bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    out.push(char::from(*byte))
                }
                other => out.push_str(&format!("%{other:02X}")),
            }
        }
        out
    }

    /// The bytes `query` takes on the request line, after the `?`, exactly as
    /// [`Self::write_request`] writes it.
    fn encoded_query_bytes(query: &[(String, String)]) -> usize {
        let pairs: usize = query
            .iter()
            .map(|(key, value)| key.len() + 1 + Self::encode_query_value(value).len())
            .sum();
        pairs + query.len().saturating_sub(1)
    }

    fn write_request(
        &self,
        stream: &mut impl Write,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&[u8]>,
    ) -> Result<(), TransportError> {
        let mut head = String::new();
        head.push_str(method.as_str());
        head.push(' ');
        head.push_str(path);
        // THE QUERY, which this transport used to drop on the floor.
        //
        // `WireRequest` has carried query parameters since the catalog routes
        // arrived, and the contract double reads them straight off the request
        // object — so every in-crate witness passed while the real transport
        // sent none of them. Against the product that meant the catalog reads
        // were never scoped to a location, the message list was never asked for
        // reading order, and a continuation never carried its cursor: the
        // client asked for page two and was handed page one, forever.
        //
        // The KEY is written as given rather than encoded. Keys here are
        // literals this crate spells — `order`, `cursor`, `location[directory]`
        // — and encoding the brackets would produce a parameter name the
        // server's own query reader does not recognise.
        for (index, (key, value)) in query.iter().enumerate() {
            head.push(if index == 0 { '?' } else { '&' });
            head.push_str(key);
            head.push('=');
            head.push_str(&Self::encode_query_value(value));
        }
        head.push_str(" HTTP/1.1\r\nHost: ");
        head.push_str(&self.address.to_string());
        head.push_str("\r\nAuthorization: ");
        head.push_str(self.credential.header_value());
        // `Connection: close` on every request. This client is not pooling, and
        // a kept-alive socket would silently spend the host's declared
        // per-connection request budget, whose exhaustion is a 503 that looks
        // exactly like overload.
        head.push_str("\r\nAccept: application/json\r\nConnection: close\r\n");
        if method == Method::Get && path == "/api/event" {
            head.push_str("X-Grapt-Approver: *\r\n");
            if let Some(id) = self.resume_header(query) {
                head.push_str(&format!("Last-Event-ID: {id}\r\n"));
            }
        }
        // This client is the one an operator drives: it always runs a window, so
        // every Session it creates is an interactive launch. Stating that at
        // creation is what stops a window that later closes from being treated
        // as the launch that may run unwatched. The server records it as a
        // durable fact; without it the Session would rely on the fallback that
        // infers a mode from window presence at the first prompt.
        if method == Method::Post && path == "/api/session" {
            head.push_str("X-Grapt-Launch: interactive\r\n");
        }
        if let Some(body) = body {
            head.push_str("Content-Type: application/json\r\nContent-Length: ");
            head.push_str(&body.len().to_string());
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        stream
            .write_all(head.as_bytes())
            .map_err(|error| TransportError::Io {
                reason: format!("write head: {error}"),
            })?;
        if let Some(body) = body {
            // VERBATIM. The bytes `crate::wire` produced are the bytes on the
            // wire; nothing here re-encodes or normalizes them.
            stream.write_all(body).map_err(|error| TransportError::Io {
                reason: format!("write body: {error}"),
            })?;
        }
        // The complete request was written; a failed flush cannot prove the
        // server did not admit it, including expiry of the socket allowance.
        stream.flush().map_err(|error| TransportError::AfterSend {
            reason: format!("flush: {error}"),
        })
    }

    /// Read the response head, returning the status, any declared length, and
    /// whatever body bytes arrived in the same read.
    fn read_head(stream: &mut impl Read) -> Result<HeadRead, TransportError> {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 1_024];
        for _ in 0..MAX_READS {
            if let Some(at) = find_terminator(&buffer) {
                return parse_head(&buffer, at);
            }
            if buffer.len() > MAX_HEAD_BYTES {
                return Err(HttpError::HeadTooLarge.into());
            }
            let count = stream
                .read(&mut chunk)
                .map_err(|error| TransportError::Io {
                    reason: format!("read head: {error}"),
                })?;
            if count == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..count]);
        }
        match find_terminator(&buffer) {
            Some(at) => parse_head(&buffer, at),
            None => Err(HttpError::HeadMalformed {
                reason: "stream ended before the head terminator".to_owned(),
            }
            .into()),
        }
    }
}

/// A parsed response head plus the body bytes that arrived with it.
struct HeadRead {
    status: u16,
    content_length: Option<usize>,
    body_prefix: Vec<u8>,
}

fn find_terminator(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_head(buffer: &[u8], at: usize) -> Result<HeadRead, TransportError> {
    let head = String::from_utf8_lossy(&buffer[..at]);
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|raw| raw.parse::<u16>().ok())
        .ok_or_else(|| HttpError::HeadMalformed {
            reason: format!("no numeric status in {status_line:?}"),
        })?;
    let mut content_length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse::<usize>().ok();
        }
    }
    Ok(HeadRead {
        status,
        content_length,
        body_prefix: buffer[at + 4..].to_vec(),
    })
}

impl HttpConnection {
    fn exchange(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&[u8]>,
        bound: usize,
        timeout: Duration,
    ) -> Result<WireResponse, TransportError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut stream = window::SocketWindow {
            stream: self.connect(timeout)?,
            deadline,
        };
        self.write_request(&mut stream, method, path, query, body)?;
        // Past this point the server may have acted on the request, so a
        // failure to hear back is not a failure to send.
        Self::receive(&mut stream, bound).map_err(|error| match error {
            TransportError::Io { reason } => TransportError::AfterSend { reason },
            other => other,
        })
    }

    /// Read one response: its head, then its bounded body.
    fn receive(stream: &mut impl Read, bound: usize) -> Result<WireResponse, TransportError> {
        let head = Self::read_head(stream)?;

        // THE PRE-ALLOCATION REFUSAL. A declared length over the bound is
        // refused here, having read only the head — the body is never read and
        // never allocated. An implementation that read first and measured
        // afterwards would satisfy the trait signature and violate its law.
        if head.content_length.is_some_and(|length| length > bound) {
            return Err(TransportError::ResponseTooLarge { bound });
        }
        if head.body_prefix.len() > bound {
            return Err(TransportError::ResponseTooLarge { bound });
        }

        let mut body = head.body_prefix;
        let target = head.content_length;
        let mut chunk = [0u8; 8_192];
        let mut closed = false;
        for _ in 0..MAX_READS {
            if target.is_some_and(|length| body.len() >= length) {
                break;
            }
            let count = stream
                .read(&mut chunk)
                .map_err(|error| TransportError::Io {
                    reason: format!("read body: {error}"),
                })?;
            if count == 0 {
                closed = true;
                break;
            }
            // Refuse at bound + 1: enough to KNOW the body overflowed, never
            // enough to be exhausted by it. A reader that accepted the whole
            // body in order to report its size would be the defect.
            if body.len() + count > bound {
                return Err(TransportError::ResponseTooLarge { bound });
            }
            body.extend_from_slice(&chunk[..count]);
        }
        if target.is_some_and(|length| body.len() < length) || (target.is_none() && !closed) {
            return Err(TransportError::Io {
                reason: "response ended before its declared body or acquisition bound".into(),
            });
        }
        if let Some(length) = target {
            body.truncate(length);
        }
        Ok(WireResponse {
            status: head.status,
            body,
        })
    }
}

impl Transport for HttpConnection {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        self.require_product_identity()?;
        self.exchange(
            request.operation().method(),
            request.path(),
            request.query(),
            request.body(),
            self.bounds.max_response_bytes,
            request_timeout(request.operation()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod after_send;

    /// A RESET subscription must not kill the client.
    ///
    /// `/api/event` is volatile by contract — a slow consumer overflows and
    /// fails the stream, and events during a disconnection are missed — so a
    /// connection that breaks abruptly is an ORDINARY event this client is
    /// promised will happen. It used to return an error here, which `pump`
    /// propagated with `?` and the app treated as fatal: measured through a
    /// relay against a real product, the native client EXITED to its banner
    /// mid-journey while the retained client carried on.
    ///
    /// A real listener is used rather than a mock, because the thing under test
    /// is what the socket layer does with a reset, and a mock would be asserting
    /// this crate's idea of that rather than the operating system's.
    #[test]
    fn a_reset_subscription_reads_as_closed_rather_than_as_an_error() {
        use std::io::Write as _;
        use std::net::TcpListener;

        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(error) => panic!("a loopback listener must bind: {error}"),
        };
        let address = match listener.local_addr() {
            Ok(address) => address,
            Err(error) => panic!("the listener must have an address: {error}"),
        };

        let server = std::thread::spawn(move || {
            let Ok((mut socket, _)) = listener.accept() else {
                return;
            };
            // Enough of a response that the reader accepts the subscription.
            let _ = socket.write_all(
                b"HTTP/1.1 200 OK
Content-Type: text/event-stream

",
            );
            let _ = socket.flush();
            // Closed with the client's REQUEST still unread. A socket closed
            // while inbound data is pending resets rather than finishing
            // politely, so the peer sees a broken connection instead of an
            // orderly end of stream — which is the case this witness is about.
            drop(socket);
        });

        let Ok(credential) = BasicCredential::new("user", "secret") else {
            panic!("credential")
        };
        let mut connection = HttpConnection::attach(address, credential, Bounds::PRODUCTION);
        connection.resubscribe();
        let _ = server.join();

        // Drain until the transport reports the end. It must REPORT it, not
        // raise it: an error here is what killed the client.
        for _ in 0..200 {
            match connection.poll_frame() {
                Ok(FramePoll::Closed) => return,
                Ok(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
                Err(error) => panic!(
                    "a reset subscription must read as Closed, not as a fatal error: {error}"
                ),
            }
        }
        panic!("the transport never reported the subscription had ended");
    }

    #[test]
    fn a_query_value_is_percent_encoded_everywhere_outside_the_unreserved_set() {
        // A cursor is an opaque server token and a location is a Windows path.
        // Encoding only what "looks dangerous" is how an `&` in a directory name
        // becomes a second query parameter, and how a `+` in a base64 cursor
        // reaches the server as a space.
        assert_eq!(
            HttpConnection::encode_query_value("C:\\work\\project a&b"),
            "C%3A%5Cwork%5Cproject%20a%26b"
        );
        assert_eq!(
            HttpConnection::encode_query_value("eyJpZCI6Im1zZ18x+/=="),
            "eyJpZCI6Im1zZ18x%2B%2F%3D%3D"
        );
        // The unreserved set survives untouched, so an ordinary value is not
        // mangled into something the server must decode twice.
        assert_eq!(
            HttpConnection::encode_query_value("asc-1.0_x~y"),
            "asc-1.0_x~y"
        );
    }

    #[test]
    fn base64_matches_the_rfc_4648_vectors() {
        // All three padding cases. An encoder wrong only at a length boundary
        // produces a credential that fails authentication with no error that
        // explains why, so the boundaries are the point of this witness.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // The exact credential DOGFOOD-1's own witness hard-codes. If this
        // disagreed, this client would be the only thing in the workspace that
        // could not authenticate against the product.
        assert_eq!(base64(b"alpha:zephyr"), "YWxwaGE6emVwaHly");
    }

    #[test]
    fn a_credential_refuses_the_shapes_that_would_forge_a_header() {
        assert_eq!(
            BasicCredential::new("al:pha", "zephyr"),
            Err(HttpError::Credential),
            "a colon in the username redefines where the secret starts"
        );
        assert_eq!(
            BasicCredential::new("alpha", "zep\r\nX-Admin: 1"),
            Err(HttpError::Credential),
            "CR/LF in a field is header injection"
        );
        assert!(BasicCredential::new("alpha", "zephyr").is_ok());
    }

    #[test]
    fn a_credential_never_renders_its_secret() {
        let Ok(credential) = BasicCredential::new("alpha", "zephyr") else {
            panic!("witness credential must construct");
        };
        let rendered = format!("{credential:?}");
        assert!(!rendered.contains("zephyr"), "{rendered}");
        assert!(!rendered.contains("YWxwaGE6emVwaHly"), "{rendered}");
    }

    #[test]
    fn a_connection_never_renders_its_secret() {
        // The `Transport` trait requires `Debug`, so this rendering is reachable
        // from any log line, panic message or error render that touches a
        // connection. It must not be a credential leak.
        let Ok(credential) = BasicCredential::new("alpha", "zephyr") else {
            panic!("witness credential must construct");
        };
        let Ok(address) = "127.0.0.1:1".parse::<SocketAddr>() else {
            panic!("witness address must parse");
        };
        let connection = HttpConnection::attach(address, credential, Bounds::PRODUCTION);
        let rendered = format!("{connection:?}");
        assert!(!rendered.contains("zephyr"), "{rendered}");
        assert!(!rendered.contains("YWxwaGE6emVwaHly"), "{rendered}");
        assert!(rendered.contains("127.0.0.1:1"), "{rendered}");
    }

    #[test]
    fn only_the_published_endpoint_spelling_is_accepted() {
        let Ok(address) = endpoint_address("http://127.0.0.1:4321") else {
            panic!("the product's own receipt spelling must resolve");
        };
        assert_eq!(address.port(), 4321);
        for bad in [
            "127.0.0.1:4321",
            "https://127.0.0.1:4321",
            "http://localhost:4321",
            "http://127.0.0.1",
            "",
        ] {
            assert!(
                endpoint_address(bad).is_err(),
                "`{bad}` must not resolve; widening this is a reviewed change"
            );
        }
    }

    #[test]
    fn a_head_is_parsed_for_status_and_declared_length() {
        let raw =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}";
        let Some(at) = find_terminator(raw) else {
            panic!("terminator must be found");
        };
        let Ok(head) = parse_head(raw, at) else {
            panic!("head must parse");
        };
        assert_eq!(head.status, 200);
        assert_eq!(head.content_length, Some(2));
        assert_eq!(head.body_prefix, b"{}".to_vec());
    }

    #[test]
    fn a_header_name_is_matched_case_insensitively() {
        // Real servers spell it several ways. Missing the length would send the
        // reader down the undeclared-length path, where it reads until close —
        // which works, and would quietly make the pre-allocation refusal
        // unreachable for every response.
        let raw = b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello";
        let Some(at) = find_terminator(raw) else {
            panic!("terminator must be found");
        };
        let Ok(head) = parse_head(raw, at) else {
            panic!("head must parse");
        };
        assert_eq!(head.content_length, Some(5));
    }
}
