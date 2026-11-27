//! Domain-neutral strict JSON decoding and the ratified wire-presence
//! vocabulary for Grapt schema owners.
//!
//! [`decode_json`] rejects oversized or over-nested input before invoking
//! serde, retains a bounded structured target path, and deliberately discards
//! serde's raw error text so rejected values do not enter logs.
//!
//! [`Required`], [`Optional`], [`Nullable`] and [`OptionalNullable`] are the
//! four ratified field categories. They hold presence privately and admit
//! domain values only through their own constructors. None implements
//! [`Default`], so the ratified prohibition on `serde(default)` is structural:
//! a container with an absent-capable field observes presence by walking its
//! wire map instead.
//!
//! This crate defines no product domain and performs no coercion, default-value
//! repair, JSONC parsing, configuration provenance, persistence, or I/O.

mod decode;
mod presence;

pub use decode::{
    decode_json, DecodeError, DecodeErrorKind, DecodeLimits, LimitError, NamedSegmentPolicy,
};
pub use presence::{
    deserialize_from_str, tri_state_ledger, Nullable, Optional, OptionalNullable, Required,
    TriStateEvidence,
};
