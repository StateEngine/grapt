//! Durable event identities. Law: `PRESERVE` — canonical event bytes and hash
//! use these exact values; no sequence as a bare `i64` at an API boundary.
//!
//! Empirical law (D1 evidence, 2026-08-08, 69,740 real events across six
//! migration eras): the durable sequence is ZERO-BASED. `EventSeq(0)` is the
//! first event of an aggregate; fixtures claiming 1-based numbering were
//! corrected against the oracle.

crate::id::prefixed_id! {
    /// Validated `evt_*` durable event identity.
    EventId, "evt"
}

/// Zero-based durable sequence within an aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EventSeq(u64);

impl EventSeq {
    pub const FIRST: EventSeq = EventSeq(0);

    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn value(self) -> u64 {
        self.0
    }

    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// Positive schema version of a durable event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EventVersion(u32);

impl EventVersion {
    /// Returns `None` for zero: version numbering starts at 1 (durable v1 is
    /// live in the oracle; v1+v2 of `session.tool.success` coexist).
    pub fn new(value: u32) -> Option<Self> {
        if value == 0 {
            None
        } else {
            Some(Self(value))
        }
    }

    pub fn value(self) -> u32 {
        self.0
    }
}

/// An aggregate-scoped monotonic writer-fencing token, issued only by the
/// store/ownership authority. Law (foundational report §"Writer-epoch and
/// legacy-event law"): the first elected Rust writer uses epoch ONE, and every
/// ownership transfer increments it transactionally; append compares the
/// submitted epoch with the currently elected epoch before allocating an
/// [`EventSeq`] or writing bytes, and a stale/unknown epoch is a typed refusal.
///
/// It is part of new event-envelope v2 canonical bytes and hashes. It is
/// deliberately DISTINCT from the database-format [`crate::migration::EngineEpoch`]:
/// that whole-database stamp cannot satisfy per-aggregate writer fencing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WriterEpoch(u32);

impl WriterEpoch {
    /// The first elected writer's epoch. Fencing begins at one; zero is not a
    /// valid elected epoch.
    pub const FIRST: WriterEpoch = WriterEpoch(1);

    /// Returns `None` for zero: an unfenced value is never a valid elected
    /// epoch. The first elected writer is [`WriterEpoch::FIRST`].
    pub fn new(value: u32) -> Option<Self> {
        if value == 0 {
            None
        } else {
            Some(Self(value))
        }
    }

    pub fn value(self) -> u32 {
        self.0
    }

    /// The epoch after an ownership transfer, or `None` at the ceiling. Checked,
    /// not saturating: an epoch that silently stopped advancing would let a lost
    /// owner's stale writes pass fencing.
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// Whether a durable event carries writer fencing. New v2 events are
/// [`EventFencing::Fenced`] with the elected [`WriterEpoch`]; TypeScript/v1
/// events imported unchanged decode as [`EventFencing::LegacyUnfenced`] and keep
/// their historical v1 canonical bytes and hashes. No new v1 (unfenced) write is
/// legal — this vocabulary is closed so the store cannot silently mint one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventFencing {
    /// A new v2 event fenced by its aggregate's elected writer epoch.
    Fenced { epoch: WriterEpoch },
    /// A v1 event imported with its original bytes/hash and no fencing epoch.
    /// Audit history only; it can never be produced by a new write.
    LegacyUnfenced,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_is_zero_based_and_monotonic() {
        assert_eq!(EventSeq::FIRST.value(), 0);
        assert_eq!(EventSeq::FIRST.next(), EventSeq::new(1));
        assert!(EventSeq::new(0) < EventSeq::new(1));
    }

    #[test]
    fn version_zero_is_unrepresentable() {
        assert_eq!(EventVersion::new(0), None);
        assert_eq!(EventVersion::new(2).map(EventVersion::value), Some(2));
    }

    #[test]
    fn writer_epoch_begins_at_one_and_fences_monotonically(
    ) -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(WriterEpoch::FIRST.value(), 1);
        // Zero is never a valid elected epoch — an unfenced value cannot pass.
        assert_eq!(WriterEpoch::new(0), None);
        assert_eq!(WriterEpoch::new(1), Some(WriterEpoch::FIRST));
        // Ownership transfer advances the epoch, checked at the ceiling.
        assert_eq!(WriterEpoch::FIRST.next(), WriterEpoch::new(2));
        assert!(WriterEpoch::FIRST < WriterEpoch::new(2).ok_or("epoch 2")?);
        assert_eq!(WriterEpoch::new(u32::MAX).and_then(WriterEpoch::next), None);
        Ok(())
    }

    #[test]
    fn fencing_distinguishes_v2_from_legacy_unfenced() {
        let fenced = EventFencing::Fenced {
            epoch: WriterEpoch::FIRST,
        };
        assert_ne!(fenced, EventFencing::LegacyUnfenced);
    }
}
