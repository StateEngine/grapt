// SPDX-License-Identifier: MIT OR Apache-2.0
//! `events.subscribe`: a cursor-resumable push stream on a connection of its
//! own. Every frame is checked against its request before it is trusted, and
//! a lagged frame or a host error ends the stream; dropping the stream closes
//! its connection, which cancels the subscription.
use crate::frame::{decode, read_frame};
use crate::wire::{IpcResponse, IpcResult};
use crate::{IpcError, API_VERSION};
use serde::{de, Deserialize, Deserializer, Serialize};
use serde_json::Value;
use tokio::io::AsyncRead;

/// The host capability that offers [`SubscribeRequest`].
pub const EVENTS_CAPABILITY: &str = "events.subscribe";
/// The smallest batch bound a subscription may run under.
pub const MIN_BATCH_BYTES: u32 = 4096;
pub(crate) const STREAM_SCHEMA: &str = "se.event-stream/v1";
const CURSOR_PREFIX: &str = "evcur_v1_";
const MAX_CURSOR_BYTES: usize = 512;

/// A position in the host's event stream. Opaque: only the host reads it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct EventCursor(String);

impl EventCursor {
    /// A cursor in the host's `evcur_v1_` form, at most 512 printable bytes.
    pub fn parse(text: &str) -> Option<Self> {
        (text.starts_with(CURSOR_PREFIX)
            && text.len() <= MAX_CURSOR_BYTES
            && text.bytes().all(|byte| byte.is_ascii_graphic()))
        .then(|| Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for EventCursor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).ok_or_else(|| de::Error::custom("an event cursor is malformed"))
    }
}

/// The kinds of event a subscription can ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventKind {
    /// A message arrived: IDs and routing only, never its body.
    #[serde(rename = "mail.notice")]
    MailNotice,
    #[serde(rename = "agent.state")]
    AgentState,
    #[serde(rename = "assignment")]
    Assignment,
    /// A decision was recorded: IDs only, never its text.
    #[serde(rename = "decision")]
    Decision,
    #[serde(rename = "session.hold")]
    SessionHold,
    #[serde(rename = "session.revoke")]
    SessionRevoke,
}

/// What to stream. An owner connection may leave `project_id` empty to
/// follow every registered project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscribeRequest {
    pub project_id: Option<String>,
    pub agent_id: Option<String>,
    /// Resume after this position; `None` starts at the head.
    pub after_cursor: Option<EventCursor>,
    pub filter: Vec<EventKind>,
    /// The bound on one frame; the host also caps it at its reply bound.
    pub max_batch_bytes: u32,
}

/// One delivered event.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub cursor: EventCursor,
    pub event_id: String,
    pub kind: EventKind,
    pub occurred_at_us: i64,
    pub payload: Value,
}

/// One frame of the stream.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamFrame {
    Event(Event),
    /// Liveness, with a head that never runs ahead of delivered events.
    Heartbeat {
        head_cursor: EventCursor,
    },
    /// Events through this cursor are gone; the stream has ended and the
    /// caller must reread current state before subscribing again.
    Lagged {
        missed_through_cursor: EventCursor,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Heartbeat {
    #[serde(rename = "kind")]
    _kind: String,
    head_cursor: EventCursor,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lagged {
    #[serde(rename = "kind")]
    _kind: String,
    missed_through_cursor: EventCursor,
}

fn frame(value: Value) -> Result<StreamFrame, serde_json::Error> {
    match value.get("kind").and_then(Value::as_str) {
        Some("heartbeat") => {
            serde_json::from_value::<Heartbeat>(value).map(|frame| StreamFrame::Heartbeat {
                head_cursor: frame.head_cursor,
            })
        }
        Some("lagged") => {
            serde_json::from_value::<Lagged>(value).map(|frame| StreamFrame::Lagged {
                missed_through_cursor: frame.missed_through_cursor,
            })
        }
        _ => serde_json::from_value(value).map(StreamFrame::Event),
    }
}

/// An open subscription.
pub struct EventStream<S> {
    stream: S,
    request_id: u64,
    generation: u64,
    limit: u32,
    resume: Option<EventCursor>,
    ended: bool,
}

impl<S> std::fmt::Debug for EventStream<S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EventStream")
            .field("request_id", &self.request_id)
            .field("limit", &self.limit)
            .field("resume", &self.resume)
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

impl<S: AsyncRead + Unpin> EventStream<S> {
    pub(crate) fn new(
        stream: S,
        request_id: u64,
        generation: u64,
        limit: u32,
        resume: Option<EventCursor>,
    ) -> Self {
        Self {
            stream,
            request_id,
            generation,
            limit,
            resume,
            ended: false,
        }
    }

    /// The next frame. A lagged frame, a host error or any failure ends the
    /// stream; later calls refuse with [`IpcError::StreamEnded`].
    pub async fn next(&mut self) -> Result<StreamFrame, IpcError> {
        if self.ended {
            return Err(IpcError::StreamEnded);
        }
        self.ended = true;
        let bytes = read_frame(&mut self.stream, self.limit).await?;
        let response: IpcResponse = decode(&bytes)?;
        if response.version != API_VERSION {
            return Err(IpcError::ResponseMismatch("protocol version"));
        }
        if response.request_id != self.request_id {
            return Err(IpcError::ResponseMismatch("request id"));
        }
        if response.generation != self.generation {
            return Err(IpcError::ResponseMismatch("host generation"));
        }
        let output = match response.result {
            IpcResult::Output(output) => output,
            IpcResult::Error(error) => return Err(IpcError::Refused(error)),
        };
        if output.schema != STREAM_SCHEMA {
            return Err(IpcError::Malformed(format!(
                "a stream frame has schema {:?}",
                output.schema
            )));
        }
        let frame = frame(output.value).map_err(|error| IpcError::Malformed(error.to_string()))?;
        match &frame {
            StreamFrame::Event(event) => self.resume = Some(event.cursor.clone()),
            StreamFrame::Heartbeat { head_cursor } => self.resume = Some(head_cursor.clone()),
            StreamFrame::Lagged { .. } => return Ok(frame),
        }
        self.ended = false;
        Ok(frame)
    }

    /// Where a new subscription resumes after this one ends: the last
    /// delivered event or heartbeat head, else where this one started.
    pub fn resume_cursor(&self) -> Option<&EventCursor> {
        self.resume.as_ref()
    }
}

#[cfg(test)]
mod tests;
