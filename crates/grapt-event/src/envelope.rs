//! Event-envelope v2: fenced canonical bytes and identity digest.
//!
//! The v2 commit identity is `{EventId, AggregateId, EventSeq}` plus the
//! aggregate-scoped [`WriterEpoch`] fencing token, and the epoch is INSIDE
//! the canonical bytes — a stale owner's envelope does not merely fail an
//! append check, it names a different fact. There is no global sequence and
//! no cross-aggregate order here.
//!
//! # No new v1 write, by construction
//!
//! [`EventEnvelopeV2::new`] takes a [`WriterEpoch`], not an
//! [`grapt_types::EventFencing`]. `LegacyUnfenced` is a DECODE result owned
//! by [`crate::legacy`]; nothing in this crate encodes an unfenced envelope,
//! and the compile-fail probe `no_unfenced_envelope` pins that the fencing
//! enum cannot be handed to the constructor. Historical kinds refuse too: a
//! new write of `session.tool.success.1` would resurrect a vocabulary the
//! manifest retired, so only `Manifest`-provenance kinds construct.
//!
//! # The encoding
//!
//! The same frozen pattern the workspace's other authority-bearing digests
//! use (`grapt-policy`'s effective-graph encoding): versioned, domain-separated, typed and
//! self-delimiting. Every value is `tag · u64 big-endian byte-length ·
//! payload`; the envelope body is an ordered sequence of its seven
//! coordinates in a fixed order that is part of the frozen format. This is
//! deliberately NOT the v1 JSON canonicalization: ECMAScript number
//! semantics were an oracle-compatibility necessity for import, not a law
//! worth carrying into new bytes. The v1 kernel remains import-only.
//!
//! The digest is a derived structure, so it is domain-labeled
//! (`secode-event-envelope/1:<hex>`) per the corrected-F1 convention — bare
//! hex stays reserved for digests over raw stored bytes.
//!
//! Two pins keep "frozen at `FORMAT_VERSION`" enforced by more than prose,
//! because every relative assertion holds just as well under a changed
//! format: `the_frozen_format_is_pinned_byte_for_byte` restates the layout
//! in literal bytes independently of the encoder, and
//! `the_golden_digest_is_pinned` freezes one known envelope's digest.
//!
//! # What the payload is here
//!
//! Opaque canonical bytes produced by the fact's owning lane. This module
//! frames and digests them; it does not interpret them, bound their
//! production (the capture bound is F2B's, admission bounds are F3's), or
//! define any domain payload struct — those land with their owners against
//! this seam.

use crate::kind::{EventKind, KindProvenance};
use grapt_types::{EventFencing, EventId, EventSeq, SessionId, Timestamp, WriterEpoch};
use sha2::{Digest as _, Sha256};
use std::fmt;
use std::str::FromStr as _;

/// The envelope encoding's frozen format version. Any structural change to
/// the byte layout bumps this and re-freezes the golden pins.
pub const FORMAT_VERSION: u64 = 1;

/// The domain label every v2 envelope stream opens with.
pub const DOMAIN: &str = "secode-event-envelope";

pub(crate) const TAG_STR: u8 = 0x01;
pub(crate) const TAG_BYTES: u8 = 0x02;
pub(crate) const TAG_U64: u8 = 0x03;
const TAG_I64: u8 = 0x04;

/// The aggregate a durable event belongs to. Closed: the legacy log is
/// sessions only (305 of 305 measured), and every other family keeps its own
/// `WriterEpoch` and `EventSeq` fencing. A further family enters only under review, where every consumer match
/// breaks until it decides what the new family means.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AggregateId {
    Session(SessionId),
    Form(grapt_types::FormId),
    Mail(grapt_types::remote_mail::MailStreamId),
}

impl AggregateId {
    /// The aggregate family label inside the canonical bytes.
    fn family(&self) -> &'static str {
        match self {
            AggregateId::Session(_) => "session",
            AggregateId::Form(_) => "form",
            AggregateId::Mail(_) => "mail",
        }
    }

    /// The aggregate identity string inside the canonical bytes.
    fn identity(&self) -> &str {
        match self {
            AggregateId::Session(id) => id.as_str(),
            AggregateId::Form(id) => id.as_str(),
            AggregateId::Mail(id) => id.as_str(),
        }
    }
}

/// A domain-labeled SHA-256 digest of one envelope's canonical bytes.
///
/// Renders as `secode-event-envelope/1:<64 hex>`. The label is part of the
/// identity: a v2 envelope digest can never be read as a raw-bytes digest or
/// as any other domain's derived digest.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct EventEnvelopeDigest([u8; 32]);

impl EventEnvelopeDigest {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for EventEnvelopeDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{DOMAIN}/{FORMAT_VERSION}:")?;
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for EventEnvelopeDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EventEnvelopeDigest({self})")
    }
}

/// The kind a v2 envelope carries: a legacy-vocabulary kind still written
/// today, or a Rust-era session, form or mail fact. Closed — a new event family
/// enters only under review, alongside its owning lane's payload contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnvelopeKind {
    /// A kind from the closed legacy vocabulary. Only `Manifest`-provenance
    /// kinds construct; historical kinds refuse (see [`EnvelopeError`]).
    Legacy(EventKind),
    /// A Session fact the Rust reconstruction declares itself.
    ///
    /// Only [`KindProvenance::RustNative`] kinds construct here, and `Legacy`
    /// refuses them — one variant per provenance class, in both directions.
    /// Riding a Rust-native kind on `Legacy` would be a mislabel in a
    /// different coat: that variant's whole meaning is "a key the frozen
    /// oracle manifest declared", and the envelope label is read as
    /// provenance exactly as the enum is.
    RustSession(EventKind),
    /// A globally owned form lifecycle fact.
    Form(EventKind),
    NativeMail(EventKind),
}

impl EnvelopeKind {
    /// The exact versioned kind string inside the canonical bytes.
    pub fn kind_str(&self) -> &'static str {
        match self {
            EnvelopeKind::Legacy(kind) | EnvelopeKind::RustSession(kind) => kind.as_str(),
            EnvelopeKind::Form(kind) | EnvelopeKind::NativeMail(kind) => kind.as_str(),
        }
    }
}

/// Why a v2 envelope refuses to construct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeError {
    /// The kind's provenance is historical. A new write of a retired kind
    /// is a new legacy event, and no new legacy event may be written.
    HistoricalKind { kind: EventKind },
    /// The kind's provenance does not match the variant that carries it.
    ///
    /// `Legacy` carries only oracle-manifest kinds and `RustSession` only
    /// Rust-native ones, so this refusal fires in BOTH directions: a
    /// Rust-native kind offered to `Legacy`, and a manifest kind offered to
    /// `RustSession`. Naming the expected and actual provenance keeps the two
    /// directions distinguishable in a receipt rather than collapsing them
    /// into one "wrong kind".
    ProvenanceMismatch {
        kind: EventKind,
        expected: KindProvenance,
        actual: KindProvenance,
    },
    /// The kind and the aggregate family disagree: a legacy-vocabulary kind
    /// is a session fact that must ride a `Session` aggregate, and each other
    /// family's kinds ride only that family's aggregate.
    KindAggregateMismatch {
        kind_family: &'static str,
        aggregate_family: &'static str,
    },
}

impl fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EnvelopeError::HistoricalKind { kind } => write!(
                f,
                "kind {kind} is historical; a new v2 write may carry only manifest kinds"
            ),
            EnvelopeError::ProvenanceMismatch {
                kind,
                expected,
                actual,
            } => write!(
                f,
                "kind {kind} has {actual:?} provenance and cannot ride an envelope \
                 variant reserved for {expected:?}"
            ),
            EnvelopeError::KindAggregateMismatch {
                kind_family,
                aggregate_family,
            } => write!(
                f,
                "a {kind_family} kind cannot ride a {aggregate_family} aggregate"
            ),
        }
    }
}

impl std::error::Error for EnvelopeError {}

/// One fenced v2 event envelope. Fields are private and the only
/// constructor requires a [`WriterEpoch`], so an unfenced v2 envelope is
/// unrepresentable rather than merely refused.
#[derive(Debug, Clone)]
pub struct EventEnvelopeV2 {
    id: EventId,
    aggregate: AggregateId,
    seq: EventSeq,
    epoch: WriterEpoch,
    kind: EnvelopeKind,
    created: Timestamp,
    payload: Vec<u8>,
}

impl EventEnvelopeV2 {
    /// Constructs a fenced envelope.
    ///
    /// `epoch` is the elected writer's fencing token — the store compares it
    /// against the currently elected epoch before allocating a sequence;
    /// this type makes it part of the fact itself. `created` comes from the
    /// caller's injected clock authority: this crate reads no ambient time.
    /// `payload` is the fact's own canonical bytes, produced by its owning
    /// lane and opaque here.
    pub fn new(
        id: EventId,
        aggregate: AggregateId,
        seq: EventSeq,
        epoch: WriterEpoch,
        kind: EnvelopeKind,
        created: Timestamp,
        payload: Vec<u8>,
    ) -> Result<Self, EnvelopeError> {
        match (&kind, &aggregate) {
            (EnvelopeKind::Legacy(legacy), AggregateId::Session(_)) => {
                // Unchanged law: `Legacy` carries oracle-manifest kinds only.
                // A historical kind keeps its own named refusal, because "this
                // key was retired" and "this key belongs to another variant"
                // are different facts about why the write is illegal.
                match legacy.provenance() {
                    KindProvenance::Manifest => {}
                    KindProvenance::RustNative => {
                        return Err(EnvelopeError::ProvenanceMismatch {
                            kind: *legacy,
                            expected: KindProvenance::Manifest,
                            actual: KindProvenance::RustNative,
                        })
                    }
                    _ => return Err(EnvelopeError::HistoricalKind { kind: *legacy }),
                }
            }
            (EnvelopeKind::RustSession(native), AggregateId::Session(_))
                if !native.is_global_form() && !native.is_native_mail() =>
            {
                // The mirror direction. Anything the oracle declared — or
                // retired — is refused here, so neither variant can quietly
                // become the other's escape hatch.
                let actual = native.provenance();
                if actual != KindProvenance::RustNative {
                    return Err(EnvelopeError::ProvenanceMismatch {
                        kind: *native,
                        expected: KindProvenance::RustNative,
                        actual,
                    });
                }
            }
            (EnvelopeKind::NativeMail(kind), AggregateId::Mail(_)) if kind.is_native_mail() => {}
            (EnvelopeKind::Form(kind), AggregateId::Form(_)) if kind.is_global_form() => {}
            (mismatched_kind, mismatched_aggregate) => {
                return Err(EnvelopeError::KindAggregateMismatch {
                    kind_family: match mismatched_kind {
                        EnvelopeKind::Legacy(_) => "legacy-session",
                        EnvelopeKind::RustSession(_) => "rust-native-session",
                        EnvelopeKind::Form(_) => "global-form",
                        EnvelopeKind::NativeMail(_) => "native-mail",
                    },
                    aggregate_family: mismatched_aggregate.family(),
                });
            }
        }
        Ok(Self {
            id,
            aggregate,
            seq,
            epoch,
            kind,
            created,
            payload,
        })
    }

    pub fn id(&self) -> &EventId {
        &self.id
    }

    pub fn aggregate(&self) -> &AggregateId {
        &self.aggregate
    }

    pub fn seq(&self) -> EventSeq {
        self.seq
    }

    pub fn epoch(&self) -> WriterEpoch {
        self.epoch
    }

    /// Always [`EventFencing::Fenced`]: the unfenced arm exists only as a
    /// decode result in [`crate::legacy`].
    pub fn fencing(&self) -> EventFencing {
        EventFencing::Fenced { epoch: self.epoch }
    }

    pub fn kind(&self) -> EnvelopeKind {
        self.kind
    }

    pub fn created(&self) -> Timestamp {
        self.created
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// The canonical v2 bytes: domain label, format version, then the seven
    /// coordinates in frozen order. Total — there is no failure mode that
    /// could be mistaken for a refusal.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(96 + self.payload.len());
        write_str(&mut out, TAG_BYTES, DOMAIN);
        write_u64(&mut out, FORMAT_VERSION);
        write_str(&mut out, TAG_STR, self.id.as_str());
        write_str(&mut out, TAG_STR, self.aggregate.family());
        write_str(&mut out, TAG_STR, self.aggregate.identity());
        write_u64(&mut out, self.seq.value());
        write_u64(&mut out, u64::from(self.epoch.value()));
        write_str(&mut out, TAG_STR, self.kind.kind_str());
        write_i64(&mut out, self.created.as_millis());
        write_bytes(&mut out, &self.payload);
        out
    }

    /// The domain-labeled digest of [`Self::canonical_bytes`].
    pub fn digest(&self) -> EventEnvelopeDigest {
        let mut hasher = Sha256::new();
        hasher.update(self.canonical_bytes());
        EventEnvelopeDigest(hasher.finalize().into())
    }
}

/// Why stored v2 canonical bytes refuse to decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeDecodeError {
    /// The stream is malformed: truncated, or a scalar carried the wrong tag.
    Malformed { at: usize },
    /// The domain label or format version is not this encoder's.
    ForeignEnvelope,
    /// Bytes remained after the envelope was fully decoded.
    TrailingBytes { remaining: usize },
    /// A coordinate failed its validating constructor.
    InvalidCoordinate { field: &'static str },
    /// The aggregate family is outside the closed set.
    UnknownFamily { family: String },
    /// The kind string is outside the closed vocabulary for its family.
    UnknownKind { kind: String },
    /// The decoded coordinates do not satisfy the kind/aggregate pairing.
    Rejected { reason: EnvelopeError },
}

impl fmt::Display for EnvelopeDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EnvelopeDecodeError::Malformed { at } => {
                write!(f, "v2 envelope bytes are malformed at byte {at}")
            }
            EnvelopeDecodeError::ForeignEnvelope => {
                f.write_str("bytes are not a grapt v2 event envelope")
            }
            EnvelopeDecodeError::TrailingBytes { remaining } => {
                write!(f, "v2 envelope has {remaining} trailing byte(s)")
            }
            EnvelopeDecodeError::InvalidCoordinate { field } => {
                write!(f, "v2 envelope has an invalid {field}")
            }
            EnvelopeDecodeError::UnknownFamily { family } => {
                write!(f, "v2 envelope names unknown aggregate family {family:?}")
            }
            EnvelopeDecodeError::UnknownKind { kind } => {
                write!(f, "v2 envelope names unknown kind {kind:?}")
            }
            EnvelopeDecodeError::Rejected { reason } => write!(f, "v2 envelope refused: {reason}"),
        }
    }
}

impl std::error::Error for EnvelopeDecodeError {}

impl From<ReadError> for EnvelopeDecodeError {
    fn from(error: ReadError) -> Self {
        let at = match error {
            ReadError::Truncated { at } | ReadError::UnexpectedTag { at, .. } => at,
        };
        EnvelopeDecodeError::Malformed { at }
    }
}

/// Decodes exactly the bytes [`EventEnvelopeV2::canonical_bytes`] produces.
///
/// A stored envelope row holds NO derived digest beside its bytes, so this
/// is how one is verified: decode it, rebuild
/// the envelope through the same validating constructor a new write uses, and
/// recompute the digest from the bytes themselves. A row whose blob does not
/// decode is not a fact this store may compare or fold.
pub fn decode_canonical(bytes: &[u8]) -> Result<EventEnvelopeV2, EnvelopeDecodeError> {
    let mut reader = Reader::new(bytes);
    if reader.bytes()? != DOMAIN.as_bytes() || reader.u64()? != FORMAT_VERSION {
        return Err(EnvelopeDecodeError::ForeignEnvelope);
    }
    let id = EventId::from_str(reader.str()?)
        .map_err(|_| EnvelopeDecodeError::InvalidCoordinate { field: "event id" })?;
    let family = reader.str()?.to_owned();
    let identity = reader.str()?;
    let aggregate = match family.as_str() {
        "session" => AggregateId::Session(identity.parse::<SessionId>().map_err(|_| {
            EnvelopeDecodeError::InvalidCoordinate {
                field: "session identity",
            }
        })?),
        "form" => AggregateId::Form(identity.parse().map_err(|_| {
            EnvelopeDecodeError::InvalidCoordinate {
                field: "form identity",
            }
        })?),
        "mail" => AggregateId::Mail(
            grapt_types::remote_mail::MailStreamId::try_new(identity).map_err(|_| {
                EnvelopeDecodeError::InvalidCoordinate {
                    field: "mail stream identity",
                }
            })?,
        ),
        _ => return Err(EnvelopeDecodeError::UnknownFamily { family }),
    };
    let seq = EventSeq::new(reader.u64()?);
    let epoch = u32::try_from(reader.u64()?)
        .ok()
        .and_then(WriterEpoch::new)
        .ok_or(EnvelopeDecodeError::InvalidCoordinate {
            field: "writer epoch",
        })?;
    let kind_str = reader.str()?;
    // The kind vocabulary is resolved per family, closed on both sides: a
    // form string on a session aggregate finds nothing here, and the
    // constructor below refuses the pairing even if it did.
    let kind = match &aggregate {
        AggregateId::Mail(_) => {
            EnvelopeKind::NativeMail(EventKind::parse(kind_str).map_err(|_| {
                EnvelopeDecodeError::UnknownKind {
                    kind: kind_str.to_owned(),
                }
            })?)
        }
        AggregateId::Form(_) => EnvelopeKind::Form(EventKind::parse(kind_str).map_err(|_| {
            EnvelopeDecodeError::UnknownKind {
                kind: kind_str.to_owned(),
            }
        })?),
        // The session vocabulary is no longer single-variant. This arm used to
        // wrap EVERY session kind as `Legacy`, which was correct while `Legacy`
        // was the only session variant. F4B1b added `RustSession` and the law
        // that each variant carries exactly one provenance class, so the
        // unconditional wrap became a decode that could not read back a fact
        // this same crate can write: a rust-native kind arrived at
        // `EventEnvelopeV2::new` labelled `Legacy` and was refused for a
        // provenance mismatch the DECODER introduced.
        //
        // Provenance is therefore what selects the variant, and the match is
        // exhaustive on purpose: a future provenance class must come here and
        // choose, rather than inherit whichever wrap happened to be written
        // first. The three backward-facing classes keep their existing route
        // exactly, so `Manifest` still decodes and the two historical classes
        // still reach `new`'s `HistoricalKind` refusal unchanged.
        AggregateId::Session(_) => {
            let kind =
                EventKind::parse(kind_str).map_err(|_| EnvelopeDecodeError::UnknownKind {
                    kind: kind_str.to_owned(),
                })?;
            match kind.provenance() {
                KindProvenance::RustNative => EnvelopeKind::RustSession(kind),
                KindProvenance::Manifest
                | KindProvenance::HistoricalLogOnly
                | KindProvenance::HistoricalDeleted => EnvelopeKind::Legacy(kind),
            }
        }
    };
    let created = Timestamp::from_millis(reader.i64()?);
    let payload = reader.bytes()?.to_vec();
    let remaining = reader.remaining();
    if remaining != 0 {
        return Err(EnvelopeDecodeError::TrailingBytes { remaining });
    }
    EventEnvelopeV2::new(id, aggregate, seq, epoch, kind, created, payload)
        .map_err(|reason| EnvelopeDecodeError::Rejected { reason })
}

fn write_str(out: &mut Vec<u8>, tag: u8, value: &str) {
    out.push(tag);
    out.extend_from_slice(&(value.len() as u64).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn write_bytes(out: &mut Vec<u8>, value: &[u8]) {
    out.push(TAG_BYTES);
    out.extend_from_slice(&(value.len() as u64).to_be_bytes());
    out.extend_from_slice(value);
}

fn write_u64(out: &mut Vec<u8>, value: u64) {
    out.push(TAG_U64);
    out.extend_from_slice(&8u64.to_be_bytes());
    out.extend_from_slice(&value.to_be_bytes());
}

fn write_i64(out: &mut Vec<u8>, value: i64) {
    out.push(TAG_I64);
    out.extend_from_slice(&8u64.to_be_bytes());
    out.extend_from_slice(&value.to_be_bytes());
}

/// Why a framed scalar could not be read.
///
/// Deliberately tiny: each decoder maps it into its own vocabulary, so the
/// framing has one implementation while the domain errors stay domain-shaped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadError {
    Truncated { at: usize },
    UnexpectedTag { at: usize, tag: u8 },
}

/// A cursor over this module's framing: one tag byte, an eight-byte
/// big-endian length, then exactly that many payload bytes.
///
/// It lives beside the writers on purpose. A reader that drifted from the
/// writer by one byte would be a second framing, and the round-trip tests
/// would be proving two different formats agree with themselves.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len() - self.at
    }

    fn scalar(&mut self, expected: u8) -> Result<&'a [u8], ReadError> {
        let start = self.at;
        let tag = *self
            .bytes
            .get(start)
            .ok_or(ReadError::Truncated { at: start })?;
        if tag != expected {
            return Err(ReadError::UnexpectedTag { at: start, tag });
        }
        let len_end = start + 1 + 8;
        let len_bytes: [u8; 8] = self
            .bytes
            .get(start + 1..len_end)
            .ok_or(ReadError::Truncated { at: start })?
            .try_into()
            .map_err(|_| ReadError::Truncated { at: start })?;
        let len = usize::try_from(u64::from_be_bytes(len_bytes))
            .map_err(|_| ReadError::Truncated { at: start })?;
        let end = len_end
            .checked_add(len)
            .ok_or(ReadError::Truncated { at: start })?;
        let value = self
            .bytes
            .get(len_end..end)
            .ok_or(ReadError::Truncated { at: start })?;
        self.at = end;
        Ok(value)
    }

    pub(crate) fn bytes(&mut self) -> Result<&'a [u8], ReadError> {
        self.scalar(TAG_BYTES)
    }

    pub(crate) fn str(&mut self) -> Result<&'a str, ReadError> {
        let at = self.at;
        std::str::from_utf8(self.scalar(TAG_STR)?).map_err(|_| ReadError::Truncated { at })
    }

    pub(crate) fn u64(&mut self) -> Result<u64, ReadError> {
        let at = self.at;
        let raw: [u8; 8] = self
            .scalar(TAG_U64)?
            .try_into()
            .map_err(|_| ReadError::Truncated { at })?;
        Ok(u64::from_be_bytes(raw))
    }

    pub(crate) fn i64(&mut self) -> Result<i64, ReadError> {
        let at = self.at;
        let raw: [u8; 8] = self
            .scalar(TAG_I64)?
            .try_into()
            .map_err(|_| ReadError::Truncated { at })?;
        Ok(i64::from_be_bytes(raw))
    }
}

#[cfg(test)]
mod tests;
