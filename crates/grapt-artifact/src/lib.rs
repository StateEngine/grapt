//! Bounded artifact capture and store (F2B).
//!
//! Law consumed, by name: root correction 12 "Artifact truth"; amendment §3.3
//! "Complete bounded capture"; the supersession rows "'Full output'" and "Large
//! feed payload"; and the F2B charter in the R1 foundation release order —
//! "pre-allocation byte/count bounds, `CaptureDisposition`, immutable exact
//! artifact write/fetch, identity/length/digest verification, redacted receipts
//! and streaming/reference decisions that never materialize an oversized value
//! first."
//!
//! Four boundaries this crate deliberately does NOT cross:
//!
//! * It canonicalizes nothing. Campaign plan bytes arrive already canonical from
//!   `grapt-schema`; storing them must be byte-identical. This crate does not
//!   depend on `grapt-schema`, so "F2B cannot re-canonicalize" is structural
//!   rather than a promise.
//! * It emits no event and builds no projection. Storing a plan artifact must not
//!   make a plan snapshot event or projection truth.
//! * It owns no retention or garbage collection. Artifact retention is one of the
//!   four independent retention policies and is later work; this crate owes only
//!   the metadata a collector would need.
//! * It never elects a writer, allocates a sequence, or opens a database
//!   transaction. The persistent store uses content-addressed filesystem objects;
//!   an explicitly selected volatile store keeps objects only in shared memory.
//!
//! ## The ordering that makes two recovery units safe
//!
//! An artifact and the durable fact that references it do NOT need one atomic
//! commit, provided the order is fixed: **commit the artifact first, then append
//! the referencing event.** An orphaned artifact is inert, collectable garbage; a
//! dangling reference is a broken durable fact. That asymmetry is the whole
//! reason the split is safe, and it is why [`ArtifactStore::capture`] is durable
//! before it returns in filesystem mode. A volatile store supports only volatile
//! referencing events: its references do not survive the last store clone.
//!
//! ## Why verification, not construction, is the seal
//!
//! `grapt_types::ArtifactRef::new` is public and unvalidated, and `ArtifactId`
//! and `ArtifactDigest` accept any non-empty string, so any caller can fabricate
//! a reference. That is not fixable from here and does not need to be: a forged
//! reference names bytes that either do not exist or do not match its claimed
//! length and digest, so it fails closed with the same uniform error as
//! corruption. The rule this imposes is absolute — **a reference's length and
//! digest are caller CLAIMS to be checked against stored bytes, never inputs to
//! a decision.**

mod capture;
mod carry_workers;
mod digest;
mod fs;
mod store;

pub use capture::{CaptureError, CapturedArtifact};
pub use carry_workers::CarryWorkers;
pub use fs::FilesystemArtifactStore;
pub use store::{
    ArtifactKind, ArtifactMetadata, ArtifactStore, CarryDigestBudget, CarryObservation, FetchError,
    ReadBudget, ReadPage, StatError, Verified,
};

/// Whether a capture may contain secret-derived material.
///
/// The secret law forbids "a stable offline equality oracle" over secret
/// material, and a bare content digest of guessable bytes is exactly that:
/// recompute the hash of a guess and compare. Hiding the bytes does not help
/// when the input space is small.
///
/// So a secret-bearing capture is addressed under a per-store key, and its
/// `Debug` reveals no length — the law names "neither payload nor
/// secret-dependent length".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Sensitivity {
    /// Content whose digest is safe to publish, and reproducible by any external
    /// tool as a plain SHA-256 of the stored bytes.
    Ordinary,
    /// Content that may be secret-derived. Addressed under the store key, and
    /// redacted more aggressively in `Debug`.
    SecretBearing,
}

impl Sensitivity {
    pub fn is_secret_bearing(self) -> bool {
        matches!(self, Sensitivity::SecretBearing)
    }
}

/// The hard pre-allocation bound, supplied BEFORE the first byte is read.
///
/// This is the first of the two distinct bounds the law names: a hard capture
/// bound at the I/O boundary, before unbounded allocation, versus the smaller
/// model/presentation projection bounds that live elsewhere. The frozen
/// oracle has only the second — its shell writes an unbounded file
/// and merely reads a bounded prefix — so this bound is new construction, not a
/// port.
///
/// The charter's "pre-allocation byte/count bounds" are both present, but only
/// the byte bound is a field here: `max_bytes` caps the artifact and
/// `read_buffer_bytes` caps the working set. The COUNT bound is the `MAX_CHUNKS`
/// ceiling on the metadata map, which is a crate constant rather than a
/// per-capture choice because it exists to keep `stat` O(1) in response bytes
/// for every caller. An earlier revision of this note named a `max_chunks`
/// field that does not exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureBound {
    max_bytes: u64,
    read_buffer_bytes: usize,
}

impl CaptureBound {
    /// The largest capture this bound admits, and the working-set size used to
    /// stream it.
    ///
    /// Filesystem capture streams with only `read_buffer_bytes` of payload
    /// buffering. Volatile capture additionally retains its admitted payload
    /// under the store's shared capacity limit. Both must be non-zero: a zero
    /// bound cannot distinguish "captured nothing" from "refused", and a zero
    /// buffer cannot make progress.
    pub fn new(max_bytes: u64, read_buffer_bytes: usize) -> Option<Self> {
        if max_bytes == 0 || read_buffer_bytes == 0 {
            return None;
        }
        Some(Self {
            max_bytes,
            read_buffer_bytes,
        })
    }

    pub fn max_bytes(self) -> u64 {
        self.max_bytes
    }

    pub fn read_buffer_bytes(self) -> usize {
        self.read_buffer_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bound_must_admit_at_least_one_byte_and_one_buffer() {
        assert!(
            CaptureBound::new(0, 8).is_none(),
            "a zero byte bound cannot separate an empty capture from a refusal"
        );
        assert!(
            CaptureBound::new(8, 0).is_none(),
            "a zero read buffer cannot make progress"
        );
        assert!(CaptureBound::new(1, 1).is_some());
    }
}
