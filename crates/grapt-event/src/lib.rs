//! Durable event definitions, versions, canonical encoding and hashing —
//! THIS CRATE'S BOUNDARY ONLY. Everything here is pure and synchronous: no
//! SQLite, no I/O (two clone-gated evidence tests aside), no election, no
//! feeds. The laws below bind values this crate constructs or decodes; they
//! are NOT enforced workspace-wide by this crate, and in particular the
//! store's own event surfaces are outside them until F3 consolidates (see
//! `F3_OBLIGATIONS.md` at the crate root).
//!
//! The v1 canonical kernel's law source is
//! `fixtures/rust-reconstruction/DB_EVIDENCE_D1_2026-08-08.md` §4, validated
//! against all 69,740 real event `data` blobs on the frozen clone (0 parse
//! failures, 0 idempotence failures, 0 ordering divergences). Those hashes
//! DEFINE Rust-era identity for imported v1 events; the oracle engine has no
//! hash column (its replay identity is deep equality) — a deliberate,
//! disclosed correction, not parity.

pub mod canonical;
pub mod compaction_call;
pub mod envelope;
pub mod kind;
pub mod legacy;
pub mod session_compaction;
pub mod session_message;
pub mod session_patch;
pub mod session_revert;
pub mod session_step_ended;
pub mod session_step_failed;
pub mod session_step_observed;
pub mod session_terminal;
pub mod session_tool_identity;
pub mod session_tool_nested;
pub mod session_tool_terminal;

pub use canonical::{canonical_hash_hex, canonical_string};
pub use envelope::{
    decode_canonical, AggregateId, EnvelopeDecodeError, EnvelopeError, EnvelopeKind,
    EventEnvelopeDigest, EventEnvelopeV2, DOMAIN, FORMAT_VERSION,
};
pub use kind::{EventKind, KindError, KindProvenance};
pub use legacy::{
    decode_for_read, decode_for_replay, decode_legacy_aggregate, LegacyAggregateError,
    LegacyDataLimit, LegacyDecodeError, LegacyEventRecord, LegacyRow, ReadOutcome,
};
pub use session_compaction::{canonical_bytes, decode, encode, PayloadError};

pub mod mail_presented;
pub mod operation_mail_presented;
pub mod request_context;

pub mod agent_message;
pub mod mail_progress;

pub mod request_cache;
