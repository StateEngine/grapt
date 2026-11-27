//! Typed, bounded advisory SSE decoding.

use std::collections::BTreeMap;

use grapt_types::SessionId;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{CliLimits, CursorRecord};

const MAX_EVENT_ID_BYTES: usize = 1_024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdvisoryDurable {
    #[serde(rename = "aggregateID")]
    aggregate_id: String,
    seq: u64,
    version: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AdvisoryEvent {
    #[serde(rename = "type")]
    kind: String,
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    created: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    durable: Option<AdvisoryDurable>,
    data: BTreeMap<String, Value>,
    #[serde(skip)]
    session: Option<SessionId>,
    #[serde(skip)]
    canonical: Vec<u8>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEvent {
    #[serde(rename = "type")]
    kind: String,
    id: String,
    #[serde(default)]
    created: Presence<f64>,
    #[serde(default)]
    durable: Presence<WireDurable>,
    data: BTreeMap<String, Value>,
    #[serde(default)]
    location: Presence<WireLocation>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireLocation {
    directory: String,
    #[serde(default, rename = "workspaceID")]
    workspace: Presence<String>,
}

impl WireLocation {
    fn validate(&self) -> Result<(), AdvisoryError> {
        use grapt_types::{LocationRef, Platform, WorkspaceId};
        if self.directory.len() > 65_536 {
            return Err(AdvisoryError::EventEnvelope);
        }
        let workspace = match &self.workspace {
            Presence::Missing => None,
            Presence::Value(value) if value.len() <= 256 => {
                Some(WorkspaceId::new(value.as_str()).ok_or(AdvisoryError::EventEnvelope)?)
            }
            _ => return Err(AdvisoryError::EventEnvelope),
        };
        // A remote server's location can use either platform's path syntax.
        LocationRef::on(Platform::Posix, &self.directory, workspace.clone())
            .or_else(|_| LocationRef::on(Platform::Windows, &self.directory, workspace))
            .map(|_| ())
            .map_err(|_| AdvisoryError::EventEnvelope)
    }
}

#[derive(Debug, Default)]
enum Presence<T> {
    #[default]
    Missing,
    Null,
    Value(T),
}

impl<'de, T> Deserialize<'de> for Presence<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<T>::deserialize(deserializer).map(|value| value.map_or(Self::Null, Self::Value))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDurable {
    #[serde(rename = "aggregateID")]
    aggregate_id: String,
    seq: u64,
    version: u8,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AdvisoryError {
    #[error("SSE frame is empty or exceeds its finite {maximum}-byte bound")]
    FrameBound { maximum: usize },
    #[error("SSE frame is not the exact message/data envelope")]
    FrameEnvelope,
    #[error("SSE event is not the closed typed event envelope")]
    EventEnvelope,
    #[error("SSE event kind is outside the admitted closure")]
    EventKind,
    #[error("SSE event identity is invalid")]
    EventIdentity,
    #[error("SSE event session identity is missing or invalid")]
    SessionIdentity,
    #[error("SSE durable reference is invalid for its event kind")]
    DurableReference,
}

impl AdvisoryEvent {
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn decode_sse(frame: &[u8], limits: CliLimits) -> Result<Self, AdvisoryError> {
        let maximum = limits.values().sse_frame_bytes;
        if frame.is_empty() || frame.len() > maximum {
            return Err(AdvisoryError::FrameBound { maximum });
        }
        let text = core::str::from_utf8(frame).map_err(|_| AdvisoryError::FrameEnvelope)?;
        let mut event_line = None;
        let mut data_line = None;
        for raw in text.trim_end_matches(['\r', '\n']).split('\n') {
            let line = raw.trim_end_matches('\r');
            if let Some(value) = line.strip_prefix("event: ") {
                if event_line.replace(value).is_some() {
                    return Err(AdvisoryError::FrameEnvelope);
                }
            } else if let Some(value) = line.strip_prefix("data: ") {
                if data_line.replace(value).is_some() {
                    return Err(AdvisoryError::FrameEnvelope);
                }
            } else {
                return Err(AdvisoryError::FrameEnvelope);
            }
        }
        if event_line != Some("message") {
            return Err(AdvisoryError::FrameEnvelope);
        }
        let data = data_line.ok_or(AdvisoryError::FrameEnvelope)?;
        let wire: WireEvent =
            serde_json::from_str(data).map_err(|_| AdvisoryError::EventEnvelope)?;
        // Scoped catalog hints use the same closed envelope. Validate its
        // optional location before the unknown-kind skip below.
        match &wire.location {
            Presence::Missing => {}
            Presence::Value(location) => location.validate()?,
            Presence::Null => return Err(AdvisoryError::EventEnvelope),
        }
        if wire.id.is_empty()
            || wire.id.len() > MAX_EVENT_ID_BYTES
            || wire.id.chars().any(char::is_whitespace)
        {
            return Err(AdvisoryError::EventIdentity);
        }
        let durable_kind = match wire.kind.as_str() {
            "server.connected"
            | "session.reasoning.delta"
            | "session.text.delta"
            | "session.usage.updated"
            | "session.tool.progress"
            | "websearch.updated" => false,
            "permission.asked"
            | "permission.replied"
            | "form.created"
            | "form.replied"
            | "form.cancelled"
            | "form.suspended"
            | "session.model.selected"
            | "session.agent.selected"
            | "session.subagent-model.selected"
            | "session.subagent-model.cleared"
            | "session.input.admitted"
            | "session.input.promoted"
            | "session.step.started"
            | "session.reasoning.started"
            | "session.reasoning.ended"
            | "session.text.started"
            | "session.text.ended"
            | "session.tool.input.started"
            | "session.tool.called"
            | "session.tool.success"
            | "session.tool.failed"
            | "session.execution.started"
            | "session.execution.succeeded"
            | "session.execution.failed"
            | "session.execution.interrupted"
            // Published whenever any compaction commits, automatic or
            // requested; refusing it ended every ACP turn that compacted.
            | "session.compaction.ended" => true,
            _ => return Err(AdvisoryError::EventKind),
        };
        let (created, durable) = match (wire.kind.as_str(), wire.created, wire.durable) {
            ("server.connected", Presence::Missing, Presence::Missing) => (None, None),
            (_, Presence::Value(created), Presence::Value(durable)) if durable_kind => (
                Some(created),
                Some(AdvisoryDurable {
                    aggregate_id: durable.aggregate_id,
                    seq: durable.seq,
                    version: durable.version,
                }),
            ),
            (_, Presence::Value(created), Presence::Missing) if !durable_kind => {
                (Some(created), None)
            }
            _ => return Err(AdvisoryError::DurableReference),
        };
        if created.is_some_and(|value| !value.is_finite()) {
            return Err(AdvisoryError::DurableReference);
        }
        if durable.as_ref().is_some_and(|reference| {
            reference.aggregate_id.is_empty()
                || reference.aggregate_id.len() > 256
                || exact_version(&wire.kind) != Some(reference.version)
        }) {
            return Err(AdvisoryError::DurableReference);
        }
        let session_value = wire
            .data
            .get("sessionID")
            .or_else(|| wire.data.get("form").and_then(|form| form.get("sessionID")));
        let session = session_value
            .map(|value| {
                value
                    .as_str()
                    .ok_or(AdvisoryError::SessionIdentity)?
                    .parse::<SessionId>()
                    .map_err(|_| AdvisoryError::SessionIdentity)
            })
            .transpose()?;
        // Global notices carry no Session; every other kind carries exactly
        // one. The web-search notice is global (`WebSearchUpdatedData` is
        // empty on the wire).
        let global = matches!(wire.kind.as_str(), "server.connected" | "websearch.updated");
        if global == session.is_some() {
            return Err(AdvisoryError::SessionIdentity);
        }
        Ok(Self {
            kind: wire.kind,
            id: wire.id,
            created,
            durable,
            data: wire.data,
            session,
            canonical: data.as_bytes().to_vec(),
        })
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    pub fn session_id(&self) -> Option<&SessionId> {
        self.session.as_ref()
    }

    pub(crate) fn data(&self) -> &BTreeMap<String, Value> {
        &self.data
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "session.execution.succeeded"
                | "session.execution.failed"
                | "session.execution.interrupted"
        )
    }

    pub const fn requires_durable_relist(&self) -> bool {
        true
    }

    pub fn cursor_record(&self) -> Result<Option<CursorRecord>, AdvisoryError> {
        self.durable
            .as_ref()
            .map(|reference| {
                CursorRecord::try_new(self.id.clone(), reference.seq, self.canonical.clone())
                    .map_err(|_| AdvisoryError::DurableReference)
            })
            .transpose()
    }
}

fn exact_version(kind: &str) -> Option<u8> {
    match kind {
        "permission.asked"
        | "permission.replied"
        | "form.created"
        | "form.replied"
        | "form.cancelled"
        | "form.suspended"
        | "session.tool.success"
        | "session.tool.failed" => Some(2),
        "session.model.selected"
        | "session.agent.selected"
        | "session.subagent-model.selected"
        | "session.subagent-model.cleared"
        | "session.input.admitted"
        | "session.input.promoted"
        | "session.step.started"
        | "session.reasoning.started"
        | "session.reasoning.ended"
        | "session.text.started"
        | "session.text.ended"
        | "session.tool.input.started"
        | "session.tool.called"
        | "session.execution.started"
        | "session.execution.succeeded"
        | "session.execution.failed"
        | "session.execution.interrupted"
        | "session.compaction.ended" => Some(1),
        _ => None,
    }
}
