//! A deterministic evaluator work budget.
//!
//! ADDED IN THIS VENDORED COPY. Not upstream — see `PROVENANCE.md`.
//!
//! The backtracking evaluator is worst-case exponential, which JavaScript's own
//! RegExp is too. A wall-clock timeout is not a bound on it: a runaway match
//! cannot be stopped from outside the engine, so a timeout observes an overrun
//! without ending it. This charges the evaluator for work as it performs it and
//! stops it deterministically when the budget is spent.
//!
//! Determinism matters as much as the bound. The same pattern against the same
//! input spends the same budget on every run and on every machine, because units
//! are charged per operation performed rather than per unit of time — so an
//! exhausted verdict is reproducible rather than a function of load.
//!
//! Exhaustion is NEVER a match or a non-match. It leaves by its own path, so a
//! caller cannot mistake "I stopped looking" for "it does not match".

use core::cell::Cell;

/// Raised when the evaluator spends its whole budget without reaching a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetExhausted;

impl core::fmt::Display for BudgetExhausted {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("regular expression evaluation exceeded its work budget")
    }
}

#[cfg(feature = "std")]
impl std::error::Error for BudgetExhausted {}

/// A charge-as-you-go allowance of evaluator work units.
///
/// Interior mutability is deliberate: the scanning helpers are associated
/// functions that run while the attempter is already mutably borrowed, and a
/// shared reference lets them charge without restructuring the executor.
#[derive(Debug)]
pub struct Budget {
    remaining: Cell<u64>,
    exhausted: Cell<bool>,
}

impl Budget {
    /// An allowance of `units`.
    pub fn new(units: u64) -> Self {
        Self {
            remaining: Cell::new(units),
            exhausted: Cell::new(false),
        }
    }

    /// An allowance nothing can exhaust, for callers that opt out.
    pub fn unlimited() -> Self {
        Self::new(u64::MAX)
    }

    /// Charge `units`. Returns false once the allowance is spent.
    ///
    /// Saturating rather than wrapping: an over-charge at the boundary spends
    /// the remainder and latches, it does not wrap around into a fresh
    /// allowance.
    #[inline]
    pub fn charge(&self, units: u64) -> bool {
        if self.exhausted.get() {
            return false;
        }
        let remaining = self.remaining.get();
        if remaining < units {
            self.remaining.set(0);
            self.exhausted.set(true);
            return false;
        }
        self.remaining.set(remaining - units);
        true
    }

    /// Whether the allowance has been spent. Latches once set.
    #[inline]
    pub fn is_exhausted(&self) -> bool {
        self.exhausted.get()
    }

    /// Units left unspent.
    #[inline]
    pub fn remaining(&self) -> u64 {
        self.remaining.get()
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self::unlimited()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charging_spends_and_then_latches() {
        let budget = Budget::new(3);
        assert!(budget.charge(2));
        assert_eq!(budget.remaining(), 1);
        assert!(!budget.is_exhausted());
        // The charge that cannot be paid spends the remainder and latches.
        assert!(!budget.charge(2));
        assert_eq!(budget.remaining(), 0);
        assert!(budget.is_exhausted());
        // Latched: even an affordable charge is refused afterwards, so a
        // partially-spent run cannot resume and quietly finish.
        assert!(!budget.charge(0));
    }

    #[test]
    fn an_unlimited_budget_is_not_exhausted_by_ordinary_work() {
        let budget = Budget::unlimited();
        for _ in 0..10_000 {
            assert!(budget.charge(1));
        }
        assert!(!budget.is_exhausted());
    }
}
