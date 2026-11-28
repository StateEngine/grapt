// SPDX-License-Identifier: MIT OR Apache-2.0
//! `events.subscribe` vocabulary: what a subscription asks for, and the frames
//! its stream delivers.
use grapt_stateengine_api::{AgentId, EventId, ProjectId};
use serde::{de, Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

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
    #[serde(default, with = "crate::ids::option")]
    pub project_id: Option<ProjectId>,
    #[serde(default, with = "crate::ids::option")]
    pub agent_id: Option<AgentId>,
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
    #[serde(with = "crate::ids")]
    pub event_id: EventId,
    pub kind: EventKind,
    pub occurred_at_us: i64,
    /// Always an object; a scalar payload refuses the frame.
    pub payload: Map<String, Value>,
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

pub(crate) fn frame(value: Value) -> Result<StreamFrame, serde_json::Error> {
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
