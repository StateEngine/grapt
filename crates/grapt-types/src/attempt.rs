//! Physical attempt identity. Law: `CORRECT` — an attempt is minted once per
//! actual `llm.stream()` call (oracle invariant `llm.ts:355`, independently
//! reproduced by the red-team review) and is never reused by a retry.

crate::id::opaque_id! {
    /// Attempt identity, minted once per physical provider stream.
    PhysicalAttemptId
}

crate::id::opaque_id! {
    /// Process-local execution generation (opaque text in the oracle today).
    ExecutionId
}

crate::id::opaque_id! {
    /// Optional grouping identity for physical attempts that belong together —
    /// a retry chain, a best-of-N panel, or a parallel attempt group. Distinct
    /// from any single [`PhysicalAttemptId`]: a group has many attempts, and an
    /// attempt is never reused across groups.
    AttemptGroupId
}

/// The 1-based position of a physical attempt within its [`AttemptGroupId`],
/// counting retries and parallel members alike. Zero is not a valid ordinal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AttemptOrdinal(u32);

impl AttemptOrdinal {
    /// The first attempt in a group.
    pub const FIRST: AttemptOrdinal = AttemptOrdinal(1);

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

    /// The next ordinal in the group, or `None` at the ceiling.
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// Positive step number within a session run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StepNumber(u32);

impl StepNumber {
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

/// A physical attempt's typed token spend: tokens are RESERVED before the
/// stream and CHARGED once its usage is known. Spend is a durable reserve/charge
/// fact, never inferred later from other state (report row `attempt`:
/// "spend is not inferred later"). An attempt that reserved but never charged is
/// a distinguishable, honest state — not a silent zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttemptSpend {
    reserved: u64,
    charged: Option<u64>,
}

impl AttemptSpend {
    /// Reserve `tokens` for an attempt before it runs. Nothing is charged yet.
    pub fn reserve(tokens: u64) -> Self {
        Self {
            reserved: tokens,
            charged: None,
        }
    }

    /// Record the tokens actually charged once usage is known. Explicit, so a
    /// missing charge stays visibly uncharged rather than defaulting to the
    /// reservation.
    pub fn charge(self, tokens: u64) -> Self {
        Self {
            reserved: self.reserved,
            charged: Some(tokens),
        }
    }

    pub fn reserved(self) -> u64 {
        self.reserved
    }

    /// The tokens charged, or `None` if the attempt has not settled its spend.
    pub fn charged(self) -> Option<u64> {
        self.charged
    }
}

#[cfg(test)]
mod tests {
    use super::{AttemptOrdinal, AttemptSpend, StepNumber};

    #[test]
    fn step_zero_is_unrepresentable() {
        assert!(StepNumber::new(0).is_none());
        assert_eq!(StepNumber::new(1).map(StepNumber::value), Some(1));
    }

    #[test]
    fn attempt_ordinal_is_one_based_and_advances() {
        assert_eq!(AttemptOrdinal::FIRST.value(), 1);
        assert!(AttemptOrdinal::new(0).is_none());
        assert_eq!(AttemptOrdinal::FIRST.next(), AttemptOrdinal::new(2));
        assert_eq!(
            AttemptOrdinal::new(u32::MAX).and_then(AttemptOrdinal::next),
            None
        );
    }

    #[test]
    fn spend_reserves_then_charges_and_uncharged_is_not_zero() {
        let reserved = AttemptSpend::reserve(1000);
        assert_eq!(reserved.reserved(), 1000);
        assert_eq!(
            reserved.charged(),
            None,
            "an unsettled spend is not a zero charge"
        );
        let charged = reserved.charge(742);
        assert_eq!(charged.charged(), Some(742));
        assert_eq!(charged.reserved(), 1000);
    }
}
