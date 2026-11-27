//! v1 (`LegacyUnfenced`) decode semantics.
//!
//! The stored v1 row is six columns — `{id, aggregate_id, seq, created, type,
//! data}` — and nothing else. The oracle's PUBLIC envelope also carried
//! `location` and `metadata`, but neither has a column: they were dropped at
//! persist and are unrecoverable, so they have no field here either. This
//! module decodes rows; it does no I/O — SQLite iteration belongs to the
//! store.
//!
//! # Identity, honestly named
//!
//! The oracle NEVER computed a hash over an event: its replay identity is
//! key-order-insensitive deep equality, and its stored bytes are plain
//! `JSON.stringify` output. [`LegacyEventRecord::derived_identity_hash`] is
//! therefore the D1-validated canonical hash computed AT IMPORT by
//! [`crate::canonical`] — a Rust-era identity, deliberately never called
//! "historical". The raw stored `data` text is a separate fact the importing
//! store preserves verbatim; this record carries the parsed value and the
//! derived hash, and the hash is attached by the v1 kernel and never by any
//! v2 encoder.
//!
//! # Read versus replay posture
//!
//! The oracle treats an unknown durable type asymmetrically: the write/replay
//! path dies, while the read path skips the row and advances its cursor on
//! the raw tail sequence. Both postures exist here as typed outcomes of the
//! same closed refusal — [`decode_for_replay`] rejects, [`decode_for_read`]
//! classifies the same row as a typed skip. Collapsing the two would make an
//! imported database written by a newer engine behave differently on read
//! than on replay.
//!
//! # Sequence law
//!
//! Within an aggregate the durable sequence is zero-based and
//! monotonic-with-gaps. Gaps are real and code-caused in the oracle (fork
//! prefix reservation, a retroactive migration delete, read-path skips), so
//! [`decode_legacy_aggregate`] enforces strict increase and NEVER contiguity;
//! nothing in this crate renumbers, and there is no API that could.
//!
//! # Known-defect facts
//!
//! v1 has no tool-settlement event. Tool lifecycle was expressed by repeated
//! whole-part re-emission of `message.part.updated.1` — same part, N distinct
//! event ids and sequences, each a full restatement (the oracle projector
//! compensates by subtracting the previous value before adding the new one).
//! [`re_statement_key`] classifies those rows so a consumer folds them
//! last-write-wins instead of counting settlements; a v2 settlement is NOT a
//! re-statement and classifies as `None`. Payload-internal identities such as
//! subagent-minted `msg_sub_*` compound message ids are legal shapes and are
//! not validated here: payload contracts belong to their owning lanes.

use crate::kind::{EventKind, KindError};
use grapt_types::{EventId, EventSeq, IdError, SessionId};
use serde_json::Value;
use std::fmt;
use std::str::FromStr;

/// Byte bound applied to a row's `data` text before it is materialized as a
/// parsed value. The default is measured, not guessed: the largest durable
/// payload in the frozen clone is 4,316,520 bytes (`session.input.admitted.1`,
/// D2 census over all 69,740 events), so eight MiB imports every real row
/// with headroom while refusing an unbounded one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyDataLimit {
    max_data_bytes: usize,
}

impl LegacyDataLimit {
    /// The measured default: 8 MiB.
    pub const MEASURED_DEFAULT: LegacyDataLimit = LegacyDataLimit {
        max_data_bytes: 8 * 1024 * 1024,
    };

    /// A caller-chosen bound. Zero is refused: a bound that admits nothing
    /// is a configuration error, not a policy.
    pub fn new(max_data_bytes: usize) -> Option<Self> {
        if max_data_bytes == 0 {
            None
        } else {
            Some(Self { max_data_bytes })
        }
    }

    pub fn max_data_bytes(self) -> usize {
        self.max_data_bytes
    }
}

/// One decoded v1 durable event. Constructible only through decode in this
/// module, so every value of this type has passed the closed-kind, identity,
/// sequence and bound checks.
#[derive(Debug, Clone)]
pub struct LegacyEventRecord {
    id: EventId,
    aggregate: SessionId,
    seq: EventSeq,
    created_ms: i64,
    kind: EventKind,
    data: Value,
    derived_identity_hash: String,
}

impl LegacyEventRecord {
    pub fn id(&self) -> &EventId {
        &self.id
    }

    /// The owning aggregate. Every measured aggregate is a session; a
    /// non-session aggregate id is a contract violation and refuses at
    /// decode.
    pub fn aggregate(&self) -> &SessionId {
        &self.aggregate
    }

    pub fn seq(&self) -> EventSeq {
        self.seq
    }

    /// Wall-clock epoch milliseconds as stored. Fixture rows legally carry 0.
    pub fn created_ms(&self) -> i64 {
        self.created_ms
    }

    pub fn kind(&self) -> EventKind {
        self.kind
    }

    pub fn data(&self) -> &Value {
        &self.data
    }

    /// The D1-canonical identity hash, derived at import by the v1 kernel.
    ///
    /// Rust-era identity for a row the oracle never hashed — see the module
    /// doc. It is not, and is never presented as, a historical byte fact.
    pub fn derived_identity_hash(&self) -> &str {
        &self.derived_identity_hash
    }
}

/// Why a v1 row refuses to decode. No defaulting fallback exists anywhere:
/// the oracle's `?? "process-loss"` class of repair is illegal here.
#[derive(Debug)]
pub enum LegacyDecodeError {
    /// The `id` column is not a valid `evt_*` identity.
    Id(IdError),
    /// The `aggregate_id` column is not a valid session identity.
    Aggregate(IdError),
    /// The durable sequence is negative (the law is zero-based).
    NegativeSequence { id: EventId, seq: i64 },
    /// The `type` column is outside the closed vocabulary.
    Kind { id: EventId, error: KindError },
    /// The `data` text exceeds the configured bound. Deliberately reports
    /// the CONFIGURED limit and never the observed size, so the refusal
    /// itself discloses nothing about the oversized content.
    OversizedData { id: EventId, limit: usize },
    /// The `data` text is not valid JSON.
    Data {
        id: EventId,
        source: serde_json::Error,
    },
}

impl fmt::Display for LegacyDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LegacyDecodeError::Id(e) => write!(f, "event id: {e}"),
            LegacyDecodeError::Aggregate(e) => write!(f, "aggregate id: {e}"),
            LegacyDecodeError::NegativeSequence { id, seq } => {
                write!(f, "event {id:?}: negative durable sequence {seq}")
            }
            LegacyDecodeError::Kind { id, error } => write!(f, "event {id:?}: {error}"),
            LegacyDecodeError::OversizedData { id, limit } => {
                write!(
                    f,
                    "event {id:?}: data exceeds the configured {limit}-byte bound"
                )
            }
            LegacyDecodeError::Data { id, source } => {
                write!(f, "event {id:?}: data is not valid JSON: {source}")
            }
        }
    }
}

impl std::error::Error for LegacyDecodeError {}

/// The read path's typed outcome: a decoded record, or a typed skip whose
/// cursor still advances. Silence is not an option in this vocabulary.
#[derive(Debug)]
pub enum ReadOutcome {
    Decoded(Box<LegacyEventRecord>),
    /// The row's kind is outside the closed vocabulary. The oracle read path
    /// skips exactly this class (and only this class) while its cursor
    /// advances on the raw tail sequence.
    SkippedUnknownKind {
        seq: EventSeq,
        error: KindError,
    },
}

/// Decodes one row for REPLAY: every violation, including an unknown kind,
/// is a typed rejection.
pub fn decode_for_replay(
    id: &str,
    aggregate_id: &str,
    seq: i64,
    created_ms: i64,
    kind_raw: &str,
    data_raw: &str,
    limit: LegacyDataLimit,
) -> Result<LegacyEventRecord, LegacyDecodeError> {
    let event_id = EventId::from_str(id).map_err(LegacyDecodeError::Id)?;
    let aggregate = aggregate_id
        .parse::<SessionId>()
        .map_err(LegacyDecodeError::Aggregate)?;
    let seq =
        u64::try_from(seq)
            .map(EventSeq::new)
            .map_err(|_| LegacyDecodeError::NegativeSequence {
                id: event_id.clone(),
                seq,
            })?;
    let kind = EventKind::parse(kind_raw).map_err(|error| LegacyDecodeError::Kind {
        id: event_id.clone(),
        error,
    })?;
    // The bound is checked BEFORE the text is parsed, so an oversized row is
    // refused without materializing a value from it.
    if data_raw.len() > limit.max_data_bytes() {
        return Err(LegacyDecodeError::OversizedData {
            id: event_id,
            limit: limit.max_data_bytes(),
        });
    }
    let data: Value = serde_json::from_str(data_raw).map_err(|source| LegacyDecodeError::Data {
        id: event_id.clone(),
        source,
    })?;
    // The v1 kernel, never any v2 encoder: this hash must keep reproducing
    // the D1 era pins byte for byte.
    let derived_identity_hash = crate::canonical::canonical_hash_hex(&data);
    Ok(LegacyEventRecord {
        id: event_id,
        aggregate,
        seq,
        created_ms,
        kind,
        data,
        derived_identity_hash,
    })
}

/// Decodes one row for READ: an unknown kind becomes a typed skip whose
/// sequence still advances the caller's cursor; every other violation is the
/// same rejection replay makes. This asymmetry is the oracle's, preserved on
/// purpose — see the module doc.
pub fn decode_for_read(
    id: &str,
    aggregate_id: &str,
    seq: i64,
    created_ms: i64,
    kind_raw: &str,
    data_raw: &str,
    limit: LegacyDataLimit,
) -> Result<ReadOutcome, LegacyDecodeError> {
    match decode_for_replay(id, aggregate_id, seq, created_ms, kind_raw, data_raw, limit) {
        Ok(record) => Ok(ReadOutcome::Decoded(Box::new(record))),
        Err(LegacyDecodeError::Kind { id, error }) => match u64::try_from(seq) {
            Ok(value) => Ok(ReadOutcome::SkippedUnknownKind {
                seq: EventSeq::new(value),
                error,
            }),
            // Unreachable while replay checks the sequence before the kind,
            // but a reorder must not turn a corrupt row into a silent skip.
            Err(_) => Err(LegacyDecodeError::NegativeSequence { id, seq }),
        },
        Err(other) => Err(other),
    }
}

/// Why an aggregate slice refuses to decode as a whole.
#[derive(Debug)]
pub enum LegacyAggregateError {
    Row(LegacyDecodeError),
    /// A row belongs to a different aggregate than the slice claims.
    AggregateMismatch {
        expected: SessionId,
        found: SessionId,
        id: EventId,
    },
    /// The sequence did not strictly increase. Duplicates and reordering are
    /// refused; GAPS ARE NOT — monotonic-with-gaps is the law, and nothing
    /// here repairs or renumbers a gap.
    NonMonotonicSequence {
        previous: EventSeq,
        next: EventSeq,
        id: EventId,
    },
}

impl fmt::Display for LegacyAggregateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LegacyAggregateError::Row(e) => write!(f, "{e}"),
            LegacyAggregateError::AggregateMismatch {
                expected,
                found,
                id,
            } => write!(
                f,
                "event {id:?}: aggregate {found:?} does not match slice aggregate {expected:?}"
            ),
            LegacyAggregateError::NonMonotonicSequence { previous, next, id } => write!(
                f,
                "event {id:?}: sequence {next:?} does not strictly increase over {previous:?}"
            ),
        }
    }
}

impl std::error::Error for LegacyAggregateError {}

/// One raw v1 row, as the store reads it. A borrow-only shape so the store
/// never copies column text merely to decode it.
#[derive(Debug, Clone, Copy)]
pub struct LegacyRow<'a> {
    pub id: &'a str,
    pub aggregate_id: &'a str,
    pub seq: i64,
    pub created_ms: i64,
    pub kind: &'a str,
    pub data: &'a str,
}

/// Decodes an ordered slice of one aggregate's rows for replay, enforcing
/// same-aggregate membership and strictly-increasing sequences. Gaps pass
/// untouched: the output carries exactly the input sequences.
pub fn decode_legacy_aggregate(
    rows: &[LegacyRow<'_>],
    limit: LegacyDataLimit,
) -> Result<Vec<LegacyEventRecord>, LegacyAggregateError> {
    let mut out: Vec<LegacyEventRecord> = Vec::with_capacity(rows.len());
    for row in rows {
        let record = decode_for_replay(
            row.id,
            row.aggregate_id,
            row.seq,
            row.created_ms,
            row.kind,
            row.data,
            limit,
        )
        .map_err(LegacyAggregateError::Row)?;
        if let Some(previous) = out.last() {
            if previous.aggregate() != record.aggregate() {
                return Err(LegacyAggregateError::AggregateMismatch {
                    expected: previous.aggregate().clone(),
                    found: record.aggregate().clone(),
                    id: record.id().clone(),
                });
            }
            if record.seq() <= previous.seq() {
                return Err(LegacyAggregateError::NonMonotonicSequence {
                    previous: previous.seq(),
                    next: record.seq(),
                    id: record.id().clone(),
                });
            }
        }
        out.push(record);
    }
    Ok(out)
}

/// The identity a v1 re-statement folds on: the same key means "a later full
/// restatement of the same fact", never a second independent fact.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReStatementKey {
    aggregate: SessionId,
    part_id: String,
}

/// Classifies a decoded record as a v1 whole-part re-statement.
///
/// `Some(key)` for `message.part.updated.1` — the v1 shape with no settlement
/// event, where the last write for a key wins. `None` for everything else,
/// including v2 tool settlements, which ARE independent one-shot facts. A
/// `message.part.updated.1` row whose payload lacks `part.id` is a typed
/// refusal: the known-defect shape without its key cannot be folded safely.
pub fn re_statement_key(
    record: &LegacyEventRecord,
) -> Result<Option<ReStatementKey>, MalformedKnownDefect> {
    if record.kind() != EventKind::MessagePartUpdatedV1 {
        return Ok(None);
    }
    let part_id = record
        .data()
        .get("part")
        .and_then(|part| part.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| MalformedKnownDefect {
            id: record.id().clone(),
        })?;
    Ok(Some(ReStatementKey {
        aggregate: record.aggregate().clone(),
        part_id: part_id.to_owned(),
    }))
}

/// A `message.part.updated.1` row without a `part.id` key.
#[derive(Debug)]
pub struct MalformedKnownDefect {
    pub id: EventId,
}

impl fmt::Display for MalformedKnownDefect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "event {:?}: message.part.updated.1 payload carries no part.id",
            self.id
        )
    }
}

impl std::error::Error for MalformedKnownDefect {}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: LegacyDataLimit = LegacyDataLimit::MEASURED_DEFAULT;

    fn row<'a>(id: &'a str, seq: i64, kind: &'a str, data: &'a str) -> LegacyRow<'a> {
        LegacyRow {
            id,
            aggregate_id: "ses_a",
            seq,
            created_ms: 1_784_090_756_115,
            kind,
            data,
        }
    }

    #[test]
    fn decodes_a_versioned_row_and_derives_the_v1_hash() -> Result<(), LegacyDecodeError> {
        let record = decode_for_replay(
            "evt_a1",
            "ses_a",
            0,
            1_784_090_756_115,
            "session.tool.success.2",
            "{\"ok\":true}",
            LIMIT,
        )?;
        assert_eq!(record.kind(), EventKind::SessionToolSuccessV2);
        assert_eq!(record.seq(), EventSeq::FIRST);
        // The hash is the v1 kernel's, byte for byte.
        assert_eq!(
            record.derived_identity_hash(),
            crate::canonical::canonical_hash_hex(record.data())
        );
        Ok(())
    }

    #[test]
    fn gaps_survive_decode_untouched_and_unrepaired() -> Result<(), LegacyAggregateError> {
        // W6: fork prefix reservation makes 0..parentSeq legally empty, so a
        // slice starting at 5 with a jump to 17 is REAL history.
        let rows = [
            row("evt_a1", 5, "session.input.admitted.1", "{}"),
            row("evt_a2", 6, "session.input.promoted.1", "{}"),
            row("evt_a3", 17, "session.step.started.1", "{}"),
        ];
        let records = decode_legacy_aggregate(&rows, LIMIT)?;
        let seqs: Vec<u64> = records.iter().map(|r| r.seq().value()).collect();
        assert_eq!(seqs, vec![5, 6, 17], "sequences must pass through exactly");
        Ok(())
    }

    #[test]
    fn duplicate_and_decreasing_sequences_refuse() {
        let rows = [
            row("evt_a1", 3, "session.input.admitted.1", "{}"),
            row("evt_a2", 3, "session.input.promoted.1", "{}"),
        ];
        assert!(matches!(
            decode_legacy_aggregate(&rows, LIMIT),
            Err(LegacyAggregateError::NonMonotonicSequence { .. })
        ));
    }

    #[test]
    fn read_skips_an_unknown_kind_with_cursor_advancing_while_replay_rejects() {
        // W13: the same row, two postures.
        let args = ("evt_a1", "ses_a", 4, 0, "session.future.3", "{}");
        assert!(matches!(
            decode_for_replay(args.0, args.1, args.2, args.3, args.4, args.5, LIMIT),
            Err(LegacyDecodeError::Kind { .. })
        ));
        match decode_for_read(args.0, args.1, args.2, args.3, args.4, args.5, LIMIT) {
            Ok(ReadOutcome::SkippedUnknownKind { seq, .. }) => {
                assert_eq!(seq.value(), 4, "the skip must still advance the cursor");
            }
            other => panic!("expected a typed skip, got {other:?}"),
        }
        // Corrupt JSON aborts BOTH postures: only unknown kinds skip.
        assert!(decode_for_read(
            "evt_a1",
            "ses_a",
            4,
            0,
            "session.step.started.1",
            "{not json",
            LIMIT
        )
        .is_err());
    }

    #[test]
    fn an_oversized_data_text_refuses_naming_only_the_configured_limit() {
        let limit = match LegacyDataLimit::new(16) {
            Some(limit) => limit,
            None => panic!("16-byte limit constructs"),
        };
        let oversized = "{\"k\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}";
        let err = match decode_for_replay(
            "evt_a1",
            "ses_a",
            0,
            0,
            "session.step.started.1",
            oversized,
            limit,
        ) {
            Err(e) => e,
            Ok(_) => panic!("oversized data must refuse"),
        };
        match &err {
            LegacyDecodeError::OversizedData { limit, .. } => assert_eq!(*limit, 16),
            other => panic!("expected OversizedData, got {other:?}"),
        }
        // The rendered refusal names the configured bound, never the size.
        let rendered = err.to_string();
        assert!(rendered.contains("16-byte bound"));
        assert!(
            !rendered.contains(&oversized.len().to_string()),
            "refusal must not disclose the observed size"
        );
    }

    #[test]
    fn re_emissions_of_one_part_share_a_key_and_settlements_do_not(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // W5: v1's known-defect shape — the same part restated twice.
        let first = decode_for_replay(
            "evt_a1",
            "ses_a",
            0,
            0,
            "message.part.updated.1",
            "{\"sessionID\":\"ses_a\",\"part\":{\"id\":\"prt_1\",\"tool\":\"read\"},\"time\":1}",
            LIMIT,
        )?;
        let second = decode_for_replay(
            "evt_a2", "ses_a", 1, 0, "message.part.updated.1",
            "{\"sessionID\":\"ses_a\",\"part\":{\"id\":\"prt_1\",\"tool\":\"read\",\"state\":\"done\"},\"time\":2}",
            LIMIT,
        )?;
        let settlement = decode_for_replay(
            "evt_a3",
            "ses_a",
            2,
            0,
            "session.tool.success.2",
            "{\"callID\":\"call-1\"}",
            LIMIT,
        )?;
        let k1 = re_statement_key(&first)?.ok_or("first must classify")?;
        let k2 = re_statement_key(&second)?.ok_or("second must classify")?;
        assert_eq!(
            k1, k2,
            "one part, one fold key: re-statements, not two facts"
        );
        assert_eq!(
            re_statement_key(&settlement)?,
            None,
            "a v2 settlement is not a re-statement"
        );
        // The defect shape without its key refuses rather than folding blind.
        let keyless = decode_for_replay(
            "evt_a4",
            "ses_a",
            3,
            0,
            "message.part.updated.1",
            "{\"sessionID\":\"ses_a\"}",
            LIMIT,
        )?;
        assert!(re_statement_key(&keyless).is_err());
        Ok(())
    }
}
