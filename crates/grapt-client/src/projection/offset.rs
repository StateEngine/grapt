//! Offset admission for positioned deltas (B3 S2c). A delta that carries
//! `from`, its block's UTF-8 byte offset, applies exactly once however a
//! snapshot's provisional text and the retained stream overlap.

use super::*;

/// How the connection a frame arrived on carries live blocks: a legacy
/// stream appends deltas; a positions stream places each by its offset and
/// proves each end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamMode {
    #[default]
    Legacy,
    Positions,
}

impl SessionView {
    /// The part of a delta its open slice has not yet received, or `None` for
    /// a duplicate. On a positions stream a delta the server could not place
    /// is a hole. Every admitted byte enters the block's digest before the
    /// slice bound can discard any of it.
    pub(super) fn admit_delta(
        &mut self,
        kind: SliceKind,
        index: usize,
        envelope: &EventEnvelope,
        text: String,
        mode: StreamMode,
    ) -> Result<Option<String>, ProjectionError> {
        let from = envelope
            .data
            .get("from")
            .and_then(serde_json::Value::as_u64);
        let text = match from {
            None if mode == StreamMode::Positions => {
                return Err(ProjectionError::OffsetGap { kind });
            }
            None => text,
            Some(from) => {
                let slice = &self.slices[index];
                let received = slice.text.len().saturating_add(slice.truncated_bytes);
                match admit(received, from, &text) {
                    Offset::Append(tail) => tail.to_owned(),
                    Offset::Duplicate => return Ok(None),
                    Offset::Hole => return Err(ProjectionError::OffsetGap { kind }),
                }
            }
        };
        let ordinal = self.slices[index].ordinal;
        if let Some(wired) = self.slice_wires.get_mut(&ordinal) {
            wired.digest.update(text.as_bytes());
        }
        Ok(Some(text))
    }
}

/// What a positioned delta contributes to its open slice.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Offset<'a> {
    /// Append this text: the delta, or its unseen tail.
    Append(&'a str),
    /// Every byte was already received.
    Duplicate,
    /// Bytes before this delta never arrived, or the overlap splits a
    /// character: the block must be re-read.
    Hole,
}

/// `received` counts every byte the slice has taken, kept or truncated.
pub(super) fn admit(received: usize, from: u64, delta: &str) -> Offset<'_> {
    let Ok(from) = usize::try_from(from) else {
        return Offset::Hole;
    };
    match from.cmp(&received) {
        std::cmp::Ordering::Equal => Offset::Append(delta),
        std::cmp::Ordering::Greater => Offset::Hole,
        std::cmp::Ordering::Less => {
            let seen = received - from;
            if seen >= delta.len() {
                Offset::Duplicate
            } else {
                delta.get(seen..).map_or(Offset::Hole, Offset::Append)
            }
        }
    }
}
