//! The positions subscriber class (opt-in `positions=1`). Legacy subscribers
//! keep the single encode and offer path, byte for byte; only this class sees
//! `id: <epoch>-<seq>` lines, `from` offsets on deltas and `stream.resync`.
//! The positions themselves live in `grapt-stream`; this encodes the frames.
use super::*;
use grapt_stream::positions::{StreamPositions, POSITION_HEADROOM_BYTES};
use serde::Serialize;

/// The positioned form of one published frame. A delta whose block is
/// held is re-encoded with its offset; every other frame keeps its legacy
/// bytes. A delta without an offset is one a positions client re-reads.
pub(super) fn frame(
    positions: &mut StreamPositions,
    event: &DogfoodEvent,
    legacy: &Bytes,
    from: Option<u64>,
    limits: SseLimits,
) -> Result<Bytes, SseError> {
    match from.and_then(|from| event.positioned_delta(from)) {
        Some(delta) => positions.stamp(&encode_positioned(&delta, limits)?),
        None => positions.stamp(legacy),
    }
}

/// The session-less frame telling a positions subscriber that its stream
/// ends at a reconcile boundary and the session must be re-read.
pub(super) fn resync(
    positions: &mut StreamPositions,
    limits: SseLimits,
) -> Result<Bytes, SseError> {
    let body = encode_positioned(
        &Resync {
            kind: "stream.resync",
            data: ResyncData {
                epoch: positions.epoch(),
            },
        },
        limits,
    )?;
    positions.stamp(&body)
}

#[derive(Serialize)]
struct Resync<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    data: ResyncData<'a>,
}

#[derive(Serialize)]
struct ResyncData<'a> {
    epoch: &'a str,
}

/// Encode under the legacy event bound plus the positions headroom.
fn encode_positioned<T: Serialize>(event: &T, limits: SseLimits) -> Result<Bytes, SseError> {
    let mut values: U3aLimitValues = U3A_PRODUCTION_LIMITS;
    values.page_bytes = limits.event_bytes().saturating_add(POSITION_HEADROOM_BYTES);
    let bound = U3aLimits::try_new(values).map_err(|_| SseError::EventBytes)?;
    let json = encode_page(event, bound).map_err(|_| SseError::EventBytes)?;
    let mut frame = Vec::with_capacity(json.len().saturating_add(23));
    frame.extend_from_slice(b"event: message\ndata: ");
    frame.extend_from_slice(&json);
    frame.extend_from_slice(b"\n\n");
    Ok(Bytes::from(frame))
}
