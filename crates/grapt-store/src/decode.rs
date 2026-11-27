//! The store's durable-row decode, now a thin adapter over the accepted closed
//! decoder in `grapt_event::legacy`.
//!
//! ## What changed and why
//!
//! This module used to own a SECOND, weaker v1 semantic decode: a raw-string
//! `EventKind { name: String, version }` with an open vocabulary, no closed-set
//! check, no known-name/unknown-version distinction and no data byte bound.
//! F2A's `F3_OBLIGATIONS.md` item 1 names thinning it as F3's first store edit,
//! and the start-of-lane checks measured exactly what the open parse admitted:
//!
//! ```text
//! ".1"                     ACCEPTED -> name = ""   (an EMPTY event name)
//! "..1"                    ACCEPTED -> name = "."
//! "session.imaginary.7"    ACCEPTED -> name = "session.imaginary"
//! ```
//!
//! All three are refused by the closed decoder with precise typed errors, and
//! the real kind still decodes. The consolidation is therefore a strict
//! tightening with no behaviour on the malformed side worth preserving. The
//! store-owned `EventKind` is DELETED rather than aliased: two public types with
//! one name in one workspace is a collision a reader cannot see.
//!
//! `grapt-store` already depended on `grapt-event`, so this is a deletion
//! rather than a new dependency edge.
//!
//! ## Posture
//!
//! Every current store caller wants the REPLAY posture — an unknown kind is a
//! typed rejection, not a skip — so [`decode_row`] delegates to
//! [`grapt_event::decode_for_replay`]. The read posture (`decode_for_read`,
//! which classifies an unknown kind as a cursor-advancing skip) is deliberately
//! NOT wired into the walk here: quarantine and the read/replay split at the
//! store layer are F3B scope, and changing the walk's behaviour now would be a
//! semantic change outside this increment.

use grapt_event::{LegacyDataLimit, LegacyDecodeError};
use grapt_types::{EventId, EventSeq, EventVersion, SessionId};
use serde_json::Value;

/// The byte bound every store decode applies before parsing.
///
/// Measured, not guessed: the largest durable payload in the frozen clone is
/// 4,316,520 bytes, and the accepted default sits above it.
const STORE_DATA_LIMIT: LegacyDataLimit = LegacyDataLimit::MEASURED_DEFAULT;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// Every refusal the closed decoder can produce, carried verbatim.
    ///
    /// The variants are NOT re-spelled here. Re-stating them would let the two
    /// vocabularies drift apart again, which is the defect this module exists to
    /// remove.
    #[error("{0}")]
    Legacy(#[from] LegacyDecodeError),
    #[error("global form fact is invalid: {0}")]
    Form(&'static str),
    #[error("mail presentation is invalid: {0}")]
    MailPresented(String),
    #[error("mail progress is invalid: {0}")]
    MailProgress(String),
}

/// One decoded durable event with its derived identity hash.
///
/// `kind` is the accepted CLOSED [`grapt_event::EventKind`]. There is no
/// store-owned kind type any more.
#[derive(Debug, Clone)]
pub struct DecodedEvent {
    pub id: EventId,
    pub aggregate_id: SessionId,
    pub seq: EventSeq,
    pub created_ms: i64,
    pub kind: grapt_event::EventKind,
    pub data: Value,
    pub canonical_hash: String,
}

impl DecodedEvent {
    /// The durable event name, from the closed vocabulary.
    pub fn kind_name(&self) -> &'static str {
        self.kind.name()
    }

    /// The durable schema version, from the closed vocabulary.
    pub fn kind_version(&self) -> EventVersion {
        self.kind.version()
    }
}

/// Decodes one durable row for REPLAY. Every violation — unknown name, known
/// name at an unknown version, unversioned type, negative sequence, oversized
/// or unparseable data — is a typed rejection.
pub fn decode_row(
    id: &str,
    aggregate_id: &str,
    seq: i64,
    created_ms: i64,
    kind_raw: &str,
    data_raw: &str,
) -> Result<DecodedEvent, DecodeError> {
    if kind_raw == "session.mail.presented.1"
        && data_raw.len() > grapt_event::mail_presented::MAX_PRESENTATION_BYTES
    {
        return Err(DecodeError::MailPresented(
            "presentation exceeds its bounded codec".into(),
        ));
    }
    if kind_raw == "session.subagent-mail.presented.1"
        && data_raw.len() > crate::operation_mail::MAX_PRESENTATION_BYTES
    {
        return Err(DecodeError::MailPresented(
            "operation mail presentation exceeds its codec bound".into(),
        ));
    }
    let record = grapt_event::decode_for_replay(
        id,
        aggregate_id,
        seq,
        created_ms,
        kind_raw,
        data_raw,
        STORE_DATA_LIMIT,
    )?;
    if record.kind() == grapt_event::EventKind::SessionMailPresentedV1 {
        grapt_event::mail_presented::MailPresented::decode(record.data())
            .map_err(DecodeError::MailPresented)?;
    }
    if record.kind() == grapt_event::EventKind::SessionOperationMailPresentedV1 {
        crate::operation_mail::OperationMailPresented::decode(record.data())
            .map_err(DecodeError::MailPresented)?;
    }
    Ok(DecodedEvent {
        id: record.id().clone(),
        aggregate_id: record.aggregate().clone(),
        seq: record.seq(),
        created_ms: record.created_ms(),
        kind: record.kind(),
        data: record.data().clone(),
        canonical_hash: record.derived_identity_hash().to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four cases the start-of-lane checks executed against BOTH decoders.
    /// They are permanent witnesses here rather than a scratch probe, because
    /// the open parse they refuse is exactly what this module deleted.
    #[test]
    fn the_open_name_shapes_the_old_parse_admitted_are_now_refused() {
        // An EMPTY event name. The old `rsplit_once('.')` parse split ".1" into
        // ("", "1") and accepted it.
        let empty = decode_row("evt_a", "ses_a", 0, 0, ".1", "{}");
        assert!(
            matches!(
                empty,
                Err(DecodeError::Legacy(LegacyDecodeError::Kind { .. }))
            ),
            "an empty event name must be refused, got {empty:?}"
        );

        // A single dot as the whole name.
        let dot = decode_row("evt_a", "ses_a", 0, 0, "..1", "{}");
        assert!(
            matches!(
                dot,
                Err(DecodeError::Legacy(LegacyDecodeError::Kind { .. }))
            ),
            "\".\" is not a durable event name, got {dot:?}"
        );

        // A well-formed but UNMODELLED name. This is the passthrough the closed
        // vocabulary exists to deny.
        let unknown = decode_row("evt_a", "ses_a", 0, 0, "session.imaginary.7", "{}");
        assert!(
            matches!(
                unknown,
                Err(DecodeError::Legacy(LegacyDecodeError::Kind { .. }))
            ),
            "an unmodelled durable name must be refused, got {unknown:?}"
        );
    }

    #[test]
    fn a_real_durable_kind_still_decodes() {
        // The tightening must not cost a legal row. Asserting the refusals alone
        // would pass for a decoder that refuses everything.
        let ok = decode_row("evt_a", "ses_a", 0, 0, "session.tool.success.1", "{}");
        let event = match ok {
            Ok(event) => event,
            Err(error) => panic!("a real durable kind must decode: {error}"),
        };
        assert_eq!(event.kind_name(), "session.tool.success");
        assert_eq!(event.kind_version().value(), 1);
        assert_eq!(event.seq, EventSeq::FIRST);
    }

    #[test]
    fn a_known_name_at_an_unknown_version_is_refused_not_resolved() {
        // Separated from the unknown-NAME witness on purpose: the name here is
        // real, so only the version check can fire. A nearest-version fallback
        // would silently resolve this to v1 or v2.
        let out = decode_row("evt_a", "ses_a", 0, 0, "session.deleted.9", "{}");
        assert!(
            matches!(
                out,
                Err(DecodeError::Legacy(LegacyDecodeError::Kind { .. }))
            ),
            "a known name at an unknown version must refuse, got {out:?}"
        );
    }

    #[test]
    fn a_negative_sequence_is_refused() {
        let out = decode_row("evt_a", "ses_a", -5, 0, "session.tool.success.1", "{}");
        assert!(
            matches!(
                out,
                Err(DecodeError::Legacy(
                    LegacyDecodeError::NegativeSequence { .. }
                ))
            ),
            "a negative durable sequence must refuse, got {out:?}"
        );
    }

    #[test]
    fn unparseable_data_is_refused() {
        let out = decode_row(
            "evt_a",
            "ses_a",
            0,
            0,
            "session.tool.success.1",
            "{not json",
        );
        assert!(
            matches!(
                out,
                Err(DecodeError::Legacy(LegacyDecodeError::Data { .. }))
            ),
            "unparseable data must refuse, got {out:?}"
        );
    }

    #[test]
    fn the_store_applies_the_measured_byte_bound() {
        // The bound is part of the posture this module now inherits; without it
        // the store would parse an arbitrarily large payload before deciding.
        let huge = format!("\"{}\"", "x".repeat(STORE_DATA_LIMIT.max_data_bytes() + 1));
        let out = decode_row("evt_a", "ses_a", 0, 0, "session.tool.success.1", &huge);
        assert!(
            matches!(
                out,
                Err(DecodeError::Legacy(LegacyDecodeError::OversizedData { .. }))
            ),
            "data over the measured bound must refuse before parsing, got {out:?}"
        );
    }
}
