//! The only egress this client has.
//!
//! ## Why the currency is bytes
//!
//! [`Transport`] speaks [`WireRequest`] and [`WireResponse`], whose bodies are
//! encoded bytes and whose addresses are frozen operations. It never speaks a
//! domain type, and there is deliberately no variant of it that does.
//!
//! That is the whole enforcement mechanism for the ratified I1 row. An
//! embedded, in-process transport "may optimize delivery but not bypass the
//! boundary" — and the boundary is precisely encode/decode. A trait whose
//! currency was `SessionAggregate` would let an embedded implementation hand
//! the client a domain value that no remote transport could ever produce, and
//! every reconnect, gap and restart law in this crate would then be certifying
//! a path production never takes. Because the currency is bytes, the embedded
//! implementation must serialize exactly what the remote one serializes, and
//! the two are exercised by the same witnesses.
//!
//! The saving an embedded transport is still allowed to make is real: no
//! socket, no TCP, no HTTP framing, no port. It just does not get to skip the
//! contract.
//!
//! ## Why this trait is synchronous
//!
//! The sync/async constitution puts sockets and terminal I/O on the async side
//! and domain decisions on the synchronous side. This client takes the
//! strongest available form of that rule: its domain core — the cursor, the
//! projection, the view model — contains no `.await` at all, so no borrow,
//! guard or partially-applied fold can possibly survive one. Asynchrony lives
//! in the driver that pumps a transport implementation, which is the only
//! place a socket is touched.

use crate::bounds::Bounds;
use crate::contract::Operation;

use thiserror::Error;

mod integration;

/// One addressed request, resolved against the frozen route table.
///
/// Constructed only through [`WireRequest::new`], which resolves the path from
/// the operation. A caller cannot hand-type a path here, so a route that
/// drifts from the frozen record drifts in exactly one place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireRequest {
    operation: Operation,
    path: String,
    query: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}

impl WireRequest {
    /// Address a request whose sub-resource coordinate is already a TYPED
    /// identity. Text rendering stays inside this encoding module.
    ///
    /// Generic over the identity rather than fixed to a permission id: a form
    /// is addressed by `FormWireId` on exactly the same terms. Generic keeps
    /// the invariant that matters — the coordinate is a parsed identity, never
    /// a loose string — while admitting the second kind the frozen route set
    /// actually has. A `&str` overload here would have reopened the hole
    /// `new_typed` exists to close.
    pub fn new_typed<S: crate::wire::Spelled + ?Sized>(
        operation: Operation,
        session: Option<&crate::wire::SessionWireId>,
        request: Option<&S>,
    ) -> Result<Self, TransportError> {
        use crate::wire::Spelled;
        Self::new(operation, session, request.map(Spelled::spelled))
    }

    /// Address a request, resolving its path from the frozen table.
    ///
    /// The session scope is the TYPED identity. A request cannot be addressed
    /// from an unparsed string, so there is no route by which an identity the
    /// domain refuses reaches the wire.
    pub fn new(
        operation: Operation,
        session: Option<&crate::wire::SessionWireId>,
        request: Option<&str>,
    ) -> Result<Self, TransportError> {
        let path = operation
            .path(session, request)
            .map_err(TransportError::Contract)?;
        Ok(Self {
            operation,
            path,
            // Live diagnostics are not part of the durable replay contract.
            query: if operation == Operation::MessageList {
                vec![("diagnostics".to_owned(), "cache".to_owned())]
            } else if matches!(
                operation,
                Operation::McpList | Operation::McpInspect | Operation::McpContent
            ) {
                session
                    .map(|id| {
                        (
                            "session".to_owned(),
                            crate::wire::Spelled::spelled(id).to_owned(),
                        )
                    })
                    .into_iter()
                    .collect()
            } else {
                Vec::new()
            },
            body: None,
        })
    }

    /// Attach one query parameter.
    pub fn with_query(mut self, key: &str, value: &str) -> Self {
        self.query.push((key.to_owned(), value.to_owned()));
        self
    }

    /// Attach an encoded body.
    ///
    /// The body is BYTES, already encoded through the DTO surface. This is the
    /// boundary: a caller that wanted to pass a domain value would have to
    /// encode it first, which is exactly what a remote transport does.
    pub fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = Some(body);
        self
    }

    pub fn operation(&self) -> Operation {
        self.operation
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn query(&self) -> &[(String, String)] {
        &self.query
    }

    pub fn body(&self) -> Option<&[u8]> {
        self.body.as_deref()
    }
}

/// One response, as bytes and a status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl WireResponse {
    /// Whether the status is in the 2xx family.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The single egress seam.
///
/// Implementations: an HTTP/1 client for daemon and remote use, and an
/// in-process implementation for embedded use. Both are exercised by the same
/// witnesses, because both speak the same bytes.
pub trait Transport: std::fmt::Debug {
    /// Issue one request and read its complete bounded response.
    ///
    /// The response body is bounded by [`Bounds::max_response_bytes`] at the
    /// implementation's I/O boundary, BEFORE unbounded allocation. An
    /// implementation that read the whole body and then measured it would
    /// satisfy the signature and violate the law.
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError>;
}

/// What a single poll of an event stream yielded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FramePoll {
    /// One complete `data:` payload.
    Frame(String),
    /// One complete `data:` payload whose frame carried a stream position
    /// (`id:`), sent only to a subscription that opted into positions.
    Positioned { id: String, data: String },
    /// No frame is available yet; the stream is still open.
    Idle,
    /// A bound dropped live data. The subscription is retired; reload history.
    Gap,
    /// The stream ended. Every open stream ends exactly once.
    ///
    /// This is a first-class outcome rather than an error because a stream
    /// ending is ORDINARY: the contract says live delivery is advisory. What
    /// the client does next — reconcile from durable truth — is the same
    /// whether the end was graceful or a dropped socket, which is why both
    /// arrive here rather than one arriving as a failure.
    Closed,
}

/// Every way egress can fail.
///
/// Note what is NOT here: there is no variant for "the stream ended". A stream
/// ending is [`FramePoll::Closed`], an ordinary outcome the client reconciles
/// from, because treating an advisory transport's end as an error is what
/// produces a UI that shows a red banner for a condition the contract calls
/// normal.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TransportError {
    #[error("transport temporarily unavailable: {reason}")]
    Unavailable { reason: String },
    #[error("HTTP response answered {status}")]
    Status { status: u16 },
    #[error("request could not be addressed: {0}")]
    Contract(#[from] crate::contract::ContractError),
    #[error("transport is not connected")]
    NotConnected,
    #[error("transport failed: {reason}")]
    Io { reason: String },
    /// The request was WRITTEN, then no complete reply arrived: the server may
    /// have acted on it. Unlike [`Self::Io`], this cannot be read as "nothing
    /// happened" (a dry run: an admitted prompt looked refused).
    #[error("no reply after the request was sent: {reason}")]
    AfterSend { reason: String },
    #[error("response body exceeded the {bound}-byte bound before allocation")]
    ResponseTooLarge { bound: usize },
    #[error("stream frame exceeded the {bound}-byte bound before allocation")]
    FrameTooLarge { bound: usize },
    #[error("response was not valid UTF-8: {reason}")]
    NotUtf8 { reason: String },
}

impl TransportError {
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Unavailable { .. } | Self::NotConnected)
            || matches!(
                self,
                Self::Status {
                    status: 408 | 429 | 500 | 502 | 503 | 504
                }
            )
    }

    pub(crate) fn from_io(phase: &str, error: std::io::Error) -> Self {
        let reason = format!("{phase}: {error}");
        match error.kind() {
            std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::NotConnected
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::Interrupted => Self::Unavailable { reason },
            _ => Self::Io { reason },
        }
    }
}

/// An incremental `text/event-stream` reader.
///
/// It is a pure state machine over pushed bytes: the caller supplies whatever
/// arrived and takes whatever complete frames that produced. Keeping it pure
/// is what lets the gap, restart and duplication witnesses drive it with
/// transcripts no real socket could be made to produce on purpose — a frame
/// split mid-field, a frame that arrives one byte at a time, a stream that
/// stops between a `data:` line and its terminating blank line.
///
/// It enforces [`Bounds::max_frame_bytes`] as a PRE-ALLOCATION bound: the
/// buffer is refused the moment it would exceed the bound, not after the frame
/// completes and its size can be measured.
#[derive(Debug)]
pub struct SseReader {
    bounds: Bounds,
    buffer: Vec<u8>,
    ready: std::collections::VecDeque<(Option<String>, String)>,
    overflowed: bool,
}

impl SseReader {
    pub fn new(bounds: Bounds) -> Self {
        Self {
            bounds,
            buffer: Vec::new(),
            ready: std::collections::VecDeque::new(),
            overflowed: false,
        }
    }

    /// Whether this reader has refused a frame and must be reconciled.
    ///
    /// Overflow is sticky and VISIBLE. A reader that silently resumed after
    /// dropping an oversized frame would leave the projection short exactly one
    /// event, with nothing anywhere saying so.
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// Push text, returning the number of complete frames now ready.
    pub fn push(&mut self, chunk: &str) -> Result<usize, TransportError> {
        self.push_bytes(chunk.as_bytes())
    }

    /// Keep incomplete UTF-8 in the bounded frame buffer across socket reads.
    /// Decode only complete frames, never replace a split scalar with U+FFFD.
    pub fn push_bytes(&mut self, chunk: &[u8]) -> Result<usize, TransportError> {
        let mut produced = 0;
        // A frame can finish only at a newline. Scan whole line fragments
        // instead of checking the same delimiter after every UTF-8 scalar.
        for line in chunk.split_inclusive(|byte| *byte == b'\n') {
            if line.len()
                > self
                    .bounds
                    .max_frame_bytes
                    .saturating_sub(self.buffer.len())
            {
                self.overflowed = true;
                self.clear_frame();
                return Err(TransportError::FrameTooLarge {
                    bound: self.bounds.max_frame_bytes,
                });
            }
            self.buffer.extend_from_slice(line);
            if self.buffer.ends_with(b"\n\n") || self.buffer.ends_with(b"\r\n\r\n") {
                let frame = String::from_utf8_lossy(&self.buffer);
                if let Some(payload) = Self::extract_data(&frame) {
                    if self.ready.len() < self.bounds.max_pending_events {
                        self.ready.push_back((Self::extract_id(&frame), payload));
                        produced += 1;
                    } else {
                        // Count bound reached. Same posture as the byte bound:
                        // refuse visibly rather than drop quietly.
                        self.overflowed = true;
                    }
                }
                self.clear_frame();
            }
        }
        Ok(produced)
    }

    fn clear_frame(&mut self) {
        self.buffer.clear();
        // Reuse a read-sized buffer for ordinary deltas without retaining a
        // large hydration frame for the rest of the subscription.
        self.buffer.shrink_to(8_192);
    }

    /// Take the next complete frame's payload, if any.
    pub fn take(&mut self) -> Option<String> {
        self.ready.pop_front().map(|(_, data)| data)
    }

    /// Take the next complete frame with its position, if it carried one.
    pub fn take_frame(&mut self) -> Option<FramePoll> {
        self.ready.pop_front().map(|(id, data)| match id {
            Some(id) => FramePoll::Positioned { id, data },
            None => FramePoll::Frame(data),
        })
    }

    /// The frame's last `id:` value, per the SSE grammar.
    fn extract_id(frame: &str) -> Option<String> {
        frame
            .lines()
            .filter_map(|line| line.strip_prefix("id:"))
            .map(|rest| rest.strip_prefix(' ').unwrap_or(rest).to_owned())
            .next_back()
    }

    /// Concatenate the `data:` lines of one frame per the SSE grammar.
    ///
    /// A frame carrying no `data:` line at all — a comment or a bare
    /// `event:` — yields `None` and is not a frame the client counts.
    fn extract_data(frame: &str) -> Option<String> {
        let mut data = String::new();
        let mut seen = false;
        for line in frame.lines() {
            let Some(rest) = line.strip_prefix("data:") else {
                continue;
            };
            if seen {
                data.push('\n');
            }
            seen = true;
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
        }
        seen.then_some(data)
    }
}

#[cfg(test)]
mod tests;
