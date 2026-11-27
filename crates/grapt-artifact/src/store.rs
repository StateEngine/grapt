//! The storage port, its metadata, and the bounded read contract.

use grapt_types::{ArtifactDigest, ArtifactId, ArtifactRef, CaptureDisposition, Timestamp};

use crate::{CaptureBound, CaptureError, CapturedArtifact, Sensitivity};

/// What a stored artifact is, so a consumer can reason about it without reading
/// it. Closed: adding a kind must break every exhaustive owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArtifactKind {
    /// Captured tool, process or provider output.
    CapturedOutput,
    /// A bounded, already-canonical Campaign plan artifact. F2B stores the bytes
    /// byte-identically and derives nothing from them: the canonical encoding
    /// and the graph digest belong to `grapt-schema`.
    CampaignPlan,
    /// Bounded, redacted diagnostic evidence retained by an explicit audit
    /// policy.
    Diagnostic,
}

/// Everything a reference decision needs, and no payload.
///
/// This type is the whole point of the `stat` split: the choice between
/// returning bytes inline and returning a reference is made from length alone,
/// with ZERO payload bytes read. A reference path that materializes the payload
/// before deciding to return a reference is not bounded.
#[derive(Clone, PartialEq, Eq)]
pub struct ArtifactMetadata {
    pub(crate) id: ArtifactId,
    pub(crate) length: u64,
    pub(crate) digest: ArtifactDigest,
    pub(crate) disposition: CaptureDisposition,
    pub(crate) sensitivity: Sensitivity,
    pub(crate) kind: ArtifactKind,
    pub(crate) chunk_size: u64,
    pub(crate) chunk_digests: Vec<String>,
    pub(crate) captured_at: Timestamp,
}

impl core::fmt::Debug for ArtifactMetadata {
    /// Hand-written, never derived. `missing_debug_implementations` is a warning
    /// and warnings are denied, so every type here MUST implement `Debug` — and
    /// a derive would print the digest and the exact length of secret-bearing
    /// content, which the law forbids ("neither payload nor secret-dependent
    /// length"). The lint pressures you into exactly the leak.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut out = f.debug_struct("ArtifactMetadata");
        out.field("kind", &self.kind)
            .field("sensitivity", &self.sensitivity)
            .field("disposition_complete", &self.disposition.is_complete());
        if self.sensitivity.is_secret_bearing() {
            // No id, no digest, no length: for secret-bearing content the digest
            // is an equality oracle and the length is itself secret-dependent.
            out.field("id", &"<redacted>").finish_non_exhaustive()
        } else {
            out.field("id", &self.id)
                .field("length", &self.length)
                .finish_non_exhaustive()
        }
    }
}

impl ArtifactMetadata {
    pub fn id(&self) -> &ArtifactId {
        &self.id
    }

    /// The exact stored length. This is what a reference decision reads.
    pub fn length(&self) -> u64 {
        self.length
    }

    pub fn digest(&self) -> &ArtifactDigest {
        &self.digest
    }

    /// Whether the stored bytes are the whole value or a bounded prefix.
    pub fn disposition(&self) -> CaptureDisposition {
        self.disposition
    }

    pub fn kind(&self) -> ArtifactKind {
        self.kind
    }

    pub fn sensitivity(&self) -> Sensitivity {
        self.sensitivity
    }

    pub fn captured_at(&self) -> Timestamp {
        self.captured_at
    }

    /// The reference for these bytes, minted from RECORDED state rather than
    /// from caller input. This is the only way to obtain a reference that is
    /// known to match stored bytes.
    pub fn reference(&self) -> ArtifactRef {
        ArtifactRef::new(self.id.clone(), self.length, self.digest.clone())
    }
}

/// The bound on the PAYLOAD one read returns.
///
/// It deliberately does NOT claim to bound a serialized wire response. The first
/// revision of this crate charged a fixed 256-byte "overhead" against the budget
/// and described the result as a whole-response bound; that constant was never
/// measured against any real response representation, and the pack prohibits a
/// declared-but-unenforced budget. An honest narrower bound beats a fabricated
/// wider one, so the estimate is gone rather than tuned.
///
/// **OBLIGATION RECORDED FOR THE OWNING LANE.** A real whole-response bound —
/// identifiers, verification tag, continuation and framing counted alongside the
/// payload — belongs to whoever owns the wire representation, which is the
/// downstream API/feed lane, not the storage seam. This crate bounds what it can
/// actually measure: payload bytes per page, chunk-aligned so each page is
/// independently verifiable. A page that cannot fit one chunk is a TYPED REFUSAL
/// naming the minimum, never a panic and never a silently oversized item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadBudget {
    max_payload_bytes: u64,
}

impl ReadBudget {
    pub fn new(max_payload_bytes: u64) -> Option<Self> {
        if max_payload_bytes == 0 {
            return None;
        }
        Some(Self { max_payload_bytes })
    }

    /// The most payload this read may return. Not a response bound — see the
    /// type note.
    pub fn max_payload_bytes(self) -> u64 {
        self.max_payload_bytes
    }
}

/// How much of the returned content was actually verified.
///
/// A whole-artifact digest cannot verify one page. Checking it only on the final
/// page would return every earlier page unverified while looking sealed — a
/// cosmetic seal. Per-chunk digests recorded at write time are what make a page
/// independently checkable, and this enum makes the distinction unavoidable at
/// the type level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Verified {
    /// The response is the entire artifact and its whole digest was checked.
    Whole,
    /// The response is one chunk-aligned page, checked against that chunk's own
    /// digest. The whole-artifact digest is NOT implied.
    Chunk { index: u64 },
}

/// One page of artifact content.
#[derive(Clone, PartialEq, Eq)]
pub struct ReadPage {
    pub(crate) bytes: Vec<u8>,
    pub(crate) offset: u64,
    pub(crate) verified: Verified,
    pub(crate) next_offset: Option<u64>,
}

impl core::fmt::Debug for ReadPage {
    /// Redacted: a page IS payload. Only its shape may be printed.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ReadPage")
            .field("offset", &self.offset)
            .field("bytes", &self.bytes.len())
            .field("verified", &self.verified)
            .field("next_offset", &self.next_offset)
            .finish()
    }
}

impl ReadPage {
    /// The verified bytes. Reachable only after identity, length and digest
    /// were checked against stored content.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn verified(&self) -> Verified {
        self.verified
    }

    /// Where to resume, or `None` when this page ends the artifact.
    pub fn next_offset(&self) -> Option<u64> {
        self.next_offset
    }
}

/// Why a `stat` could not answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatError {
    /// No such artifact, or its metadata is unreadable. Deliberately uniform, so
    /// `stat` is not an existence oracle over identities the caller guessed.
    Unavailable,
}

impl core::fmt::Display for StatError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("the artifact is unavailable")
    }
}

impl std::error::Error for StatError {}

/// Why a fetch refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchError {
    /// Absent, corrupt, foreign, or a reference whose claimed length, digest or
    /// disposition disagrees with what was stored.
    ///
    /// ONE uniform variant, on purpose. Splitting "no such artifact" from "your
    /// digest is wrong" would turn fetch into an existence and content oracle
    /// over guessed references.
    Unavailable,
    /// The payload budget cannot carry one chunk-aligned page. A typed refusal
    /// naming the minimum — the alternative is a panic, an unverifiable partial
    /// chunk, or an unbounded first item, and all three are worse.
    BudgetTooSmall { minimum: u64 },
    /// The requested offset is not a chunk boundary, so the page could not be
    /// independently verified.
    UnalignedOffset { chunk_size: u64 },
}

impl core::fmt::Display for FetchError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FetchError::Unavailable => f.write_str("the artifact is unavailable"),
            FetchError::BudgetTooSmall { minimum } => {
                write!(f, "a read needs at least {minimum} response bytes")
            }
            FetchError::UnalignedOffset { chunk_size } => {
                write!(
                    f,
                    "an offset must be a multiple of the {chunk_size}-byte chunk"
                )
            }
        }
    }
}

impl std::error::Error for FetchError {}

/// What an admission-time carry check observed about one referenced
/// artifact.
///
/// The reference's id, length and digest are CLAIMS. They are checked
/// against the recorded metadata, which must vouch for itself (its id
/// equals both the lookup key and the identity recomputed from kind,
/// disposition, length and digest), against the object's recorded
/// length, and - under the caller's explicit digest budget - against
/// the object's actual bytes: the stored digest is re-derived by a
/// bounded stream and compared, so a same-length byte flip is corrupt
/// at admission rather than a surprise at fetch time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarryObservation {
    /// The recorded metadata verifies against the reference, the object
    /// stands the claimed length, and its actual bytes digest to the
    /// recorded digest.
    Complete,
    /// The metadata or object file is missing: the carry lost the file.
    Absent,
    /// The file exists but cannot stand as a record: a directory, an
    /// oversized record, or an I/O failure.
    Unreadable,
    /// The record, the object, or the object's actual bytes contradict
    /// the reference's claims.
    Corrupt,
    /// The digest budget cannot carry this object's verification; the
    /// carry is refused rather than admitted half-verified.
    DigestBudgetExceeded,
}

impl core::fmt::Display for CarryObservation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            CarryObservation::Complete => "complete",
            CarryObservation::Absent => "absent",
            CarryObservation::Unreadable => "unreadable",
            CarryObservation::Corrupt => "corrupt",
            CarryObservation::DigestBudgetExceeded => "digest-budget-exceeded",
        })
    }
}

/// The explicit total byte budget for re-digesting carried objects
/// during admission.
///
/// Every verified object charges its full stored length against the
/// budget. A carry whose objects do not fit is refused with
/// [`CarryObservation::DigestBudgetExceeded`] rather than admitted
/// half-verified: an admission that skips verification is not an
/// admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CarryDigestBudget {
    total: u64,
    remaining: u64,
}

impl CarryDigestBudget {
    /// A budget of zero verifies nothing, so every object is refused
    /// with [`CarryObservation::DigestBudgetExceeded`].
    pub fn new(total: u64) -> Self {
        Self {
            total,
            remaining: total,
        }
    }

    /// The budget as given.
    pub fn total(self) -> u64 {
        self.total
    }

    /// The bytes still payable.
    pub fn remaining(self) -> u64 {
        self.remaining
    }

    /// Charge `bytes` against the budget, saturating at zero so an
    /// overdraw can never underflow.
    pub fn charge(&mut self, bytes: u64) {
        self.remaining = self.remaining.saturating_sub(bytes);
    }
}

/// The bounded artifact capture/store seam.
pub trait ArtifactStore {
    /// Capture `source` under `bound`, returning the reference paired with its
    /// disposition.
    ///
    /// Durable before it returns: the referencing event is appended afterwards,
    /// so an interrupted capture leaves collectable garbage rather than a
    /// dangling reference. Explicit in-memory stores instead commit only to
    /// shared volatile memory; their references must not enter durable events.
    fn capture<R: std::io::Read>(
        &self,
        source: R,
        bound: CaptureBound,
        kind: ArtifactKind,
        sensitivity: Sensitivity,
    ) -> Result<CapturedArtifact, CaptureError>;

    /// Everything a reference decision needs, reading ZERO payload bytes.
    fn stat(&self, id: &ArtifactId) -> Result<ArtifactMetadata, StatError>;

    /// Read one verified page, checking the reference against stored bytes
    /// first. A reference's length and digest are claims, never inputs.
    fn fetch(
        &self,
        reference: &ArtifactRef,
        offset: u64,
        budget: ReadBudget,
    ) -> Result<ReadPage, FetchError>;
}
