//! The capture wire vocabulary and its bounded query decode.
//!
//! Pure-move split out of `capture.rs` by the module-size ratchet (the
//! zero-exception baseline of `no_god_files`): the wire frame DTOs, the
//! live-frame analysis, the capture location law, and the query decode.
//! No behaviour changes; the stream body in `capture.rs` consumes this
//! surface exactly as before.

use std::collections::BTreeMap;

use axum::body::Bytes;
use serde::ser::{SerializeMap, Serializer};
use serde::Serialize;

use super::events::DogfoodEvent;
use super::query::{percent_decode, QueryError, MAX_QUERY_BYTES, MAX_QUERY_PAIRS};
use super::{DtoString, WireId};
use grapt_types::{
    CaptureCoordinate, CaptureInterest, EventFencing, EventSeq, SessionId, WorkspaceId,
    MAX_CAPTURE_CURSOR_TOKEN_BYTES,
};
/// Total raw query bound for the capture route. The v2 8,192-byte bound is a
/// frozen contract of `/api/event`; the capture cursor is a first-class
/// resume payload whose own codec caps it at 64 KB, so the total is the token
/// cap plus the v2 pair budget for the small keys.
pub(super) const MAX_CAPTURE_QUERY_BYTES: usize = MAX_CAPTURE_CURSOR_TOKEN_BYTES + MAX_QUERY_BYTES;

// ---------------------------------------------------------------------------
// Wire frames. All are `event: message` / `data: {json}` via the shared SSE
// codec; the `type` discriminator closes the capture vocabulary.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct CoordinateDto {
    #[serde(rename = "aggregateID")]
    pub(super) aggregate_id: String,
    pub(super) seq: u64,
    #[serde(rename = "writerEpoch")]
    pub(super) writer_epoch: u32,
}

pub(super) fn coordinate_dto(coordinate: &CaptureCoordinate) -> CoordinateDto {
    CoordinateDto {
        aggregate_id: coordinate.aggregate.as_str().to_owned(),
        seq: coordinate.seq.value(),
        writer_epoch: wire_epoch(&coordinate.fencing),
    }
}

fn wire_epoch(fencing: &EventFencing) -> u32 {
    match fencing {
        EventFencing::Fenced { epoch } => epoch.value(),
        // Zero is not an elected epoch; it names "no fencing carried".
        EventFencing::LegacyUnfenced => 0,
    }
}

/// The interest exactly as the route received it, mirrored from the token
/// grammar: `"all"`, or `{"directory": ..., "workspace": ...}` with the
/// workspace key omitted when absent.
#[derive(Debug, Clone)]
pub(super) struct InterestEcho(pub(super) CaptureInterest);

impl Serialize for InterestEcho {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.0 {
            CaptureInterest::All => serializer.serialize_str("all"),
            CaptureInterest::Location {
                directory,
                workspace,
            } => {
                let mut map = serializer.serialize_map(None)?;
                map.serialize_entry("directory", directory)?;
                if let Some(workspace) = workspace {
                    map.serialize_entry("workspace", workspace)?;
                }
                map.end()
            }
        }
    }
}

#[derive(Serialize)]
pub(super) struct ConnectedFrame {
    #[serde(rename = "type")]
    pub(super) frame_type: &'static str,
    pub(super) version: u64,
    pub(super) interest: InterestEcho,
    pub(super) cursor: String,
}

#[derive(Serialize)]
pub(super) struct ReplayEventFrame<'a> {
    #[serde(rename = "type")]
    pub(super) frame_type: &'static str,
    pub(super) coordinate: CoordinateDto,
    pub(super) event: &'a DogfoodEvent,
}

#[derive(Serialize)]
pub(super) struct SkippedFrame {
    #[serde(rename = "type")]
    pub(super) frame_type: &'static str,
    pub(super) coordinate: CoordinateDto,
    pub(super) kind: DtoString,
}

#[derive(Serialize)]
pub(super) struct CursorFrame {
    #[serde(rename = "type")]
    pub(super) frame_type: &'static str,
    pub(super) cursor: String,
}

#[derive(Serialize)]
pub(super) struct TerminatedFrame {
    #[serde(rename = "type")]
    pub(super) frame_type: &'static str,
    pub(super) reason: &'static str,
    pub(super) cursor: String,
}

// ---------------------------------------------------------------------------
// Live-frame analysis and the capture location law.
// ---------------------------------------------------------------------------

/// The durable coordinate a live frame carries, if any.
pub(super) struct FrameCoordinate {
    pub(super) aggregate: SessionId,
    pub(super) seq: EventSeq,
}

/// The optional location a live frame carries: the directory, and the
/// workspace when the frame scopes one.
pub(super) type FrameLocation = (String, Option<String>);

/// Split one v2 SSE frame into its durable coordinate and its optional
/// location. `None` means the frame has no durable coordinate (transient
/// frames) or is not decodable — the capture surface drops such frames for
/// every interest, fail-closed.
pub(super) fn frame_analysis(frame: &Bytes) -> Option<(FrameCoordinate, Option<FrameLocation>)> {
    let text = std::str::from_utf8(frame).ok()?;
    let data = text
        .strip_prefix("event: message\ndata: ")
        .and_then(|rest| rest.strip_suffix("\n\n"))?;
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    let durable = value.get("durable")?;
    let aggregate = durable
        .get("aggregateID")?
        .as_str()?
        .parse::<SessionId>()
        .ok()?;
    let seq = EventSeq::new(durable.get("seq")?.as_u64()?);
    let location = value.get("location").and_then(|location| {
        let directory = location.get("directory")?.as_str()?.to_owned();
        let workspace = location
            .get("workspaceID")
            .and_then(|workspace| workspace.as_str())
            .map(str::to_owned);
        Some((directory, workspace))
    });
    Some((FrameCoordinate { aggregate, seq }, location))
}

/// The capture location law. `All` admits every durable-coordinate frame,
/// scoped or unscoped. A `Location` interest admits unscoped frames, and
/// scoped frames only when they scope the same directory - and the same
/// workspace when the interest names one. The unscoped frames are wake
/// material, not presentation: the durable page is the sole presentation
/// source and applies this interest's location filter authoritatively, and
/// the fence advances only on rows the page presents, so an admitted frame
/// can never poison it. A frame scoped to another location still never
/// wakes the sink: its truth is never owed to this interest, and queuing it
/// would burn a bounded slot for nothing.
pub(super) fn capture_interested(
    interest: &CaptureInterest,
    location: Option<(&str, Option<&str>)>,
) -> bool {
    match interest {
        CaptureInterest::All => true,
        CaptureInterest::Location {
            directory,
            workspace,
        } => {
            let Some((frame_directory, frame_workspace)) = location else {
                return true;
            };
            if frame_directory != directory.as_str() {
                return false;
            }
            match (workspace, frame_workspace) {
                (None, _) => true,
                (Some(want), Some(have)) => want == have,
                (Some(_), None) => false,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Query decoding. The bounded-pair law of the v2 surface, replicated here
// (query.rs is not an owned path): total bytes, pair count, key length,
// duplicate and unknown fields all refuse the same way. Only the `cursor`
// value may grow to the token's own 64 KB bound.
// ---------------------------------------------------------------------------

pub(super) struct CaptureQuery {
    pub(super) interest: CaptureInterest,
    pub(super) cursor: Option<DtoString>,
}

struct CapturePairs {
    values: BTreeMap<String, String>,
}

impl CapturePairs {
    fn parse(raw: Option<&str>) -> Result<Self, QueryError> {
        let raw = raw.unwrap_or_default();
        if raw.len() > MAX_CAPTURE_QUERY_BYTES {
            return Err(QueryError::TooLarge);
        }
        let mut values = BTreeMap::new();
        if raw.is_empty() {
            return Ok(Self { values });
        }
        for (index, pair) in raw.split('&').enumerate() {
            if index >= MAX_QUERY_PAIRS {
                return Err(QueryError::TooManyPairs);
            }
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let key = percent_decode(key)?;
            let value = percent_decode(value)?;
            if key.len() > 64 || value.len() > max_value_bytes(&key) {
                return Err(QueryError::TooLarge);
            }
            if values.insert(key, value).is_some() {
                return Err(QueryError::DuplicateField);
            }
        }
        Ok(Self { values })
    }

    fn take(&mut self, key: &str) -> Option<String> {
        self.values.remove(key)
    }

    fn finish(self) -> Result<(), QueryError> {
        if self.values.is_empty() {
            Ok(())
        } else {
            Err(QueryError::UnknownField)
        }
    }
}

fn max_value_bytes(key: &str) -> usize {
    if key == "cursor" {
        MAX_CAPTURE_CURSOR_TOKEN_BYTES
    } else {
        MAX_QUERY_BYTES
    }
}

pub(super) fn capture_query(raw: Option<&str>) -> Result<CaptureQuery, QueryError> {
    let mut pairs = CapturePairs::parse(raw)?;
    let whole = pairs.take("location");
    let directory = pairs.take("location[directory]");
    let workspace = pairs.take("location[workspace]");
    let cursor = pairs.take("cursor");
    let has_nested = directory.is_some() || workspace.is_some();
    let interest = match (whole, has_nested) {
        (None, false) => CaptureInterest::All,
        (Some(value), false) if value == "null" => CaptureInterest::All,
        (Some(_), false) => return Err(QueryError::InvalidValue { field: "location" }),
        (None, true) => {
            let Some(directory) = directory else {
                return Err(QueryError::InvalidValue {
                    field: "location[workspace]",
                });
            };
            let workspace = match workspace {
                Some(value) => Some(
                    WireId::<WorkspaceId>::parse(value)
                        .ok_or(QueryError::InvalidValue {
                            field: "location[workspace]",
                        })?
                        .as_inner()
                        .as_str()
                        .to_owned(),
                ),
                None => None,
            };
            CaptureInterest::location(directory, workspace).map_err(|_| {
                QueryError::InvalidValue {
                    field: "location[directory]",
                }
            })?
        }
        (Some(_), true) => return Err(QueryError::ConflictingLocationForms),
    };
    let cursor = match cursor {
        None => None,
        Some(value) => Some(
            DtoString::try_new(value).map_err(|_| QueryError::InvalidValue { field: "cursor" })?,
        ),
    };
    pairs.finish()?;
    Ok(CaptureQuery { interest, cursor })
}
