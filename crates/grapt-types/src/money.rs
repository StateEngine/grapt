//! Monetary compatibility vocabulary.
//!
//! This module owns ONE value: the Rust counterpart of the frozen engine's
//! `Money.USD`. It is a **compatibility** type — its job is to be
//! interchangeable with a value the frozen oracle engine emits, not to be
//! the reconstruction's internal accounting model. Internal per-attempt spend
//! is [`crate::AttemptSpend`] and internal per-field usage is
//! [`crate::usage`]; neither is replaced or weakened here.
//!
//! # What the frozen contract actually says
//!
//! Oracle `430859db`, `packages/schema/src/money.ts`:
//!
//! ```text
//! export const USD = Schema.Finite.pipe(
//!   Schema.brand("Money.USD"),
//!   statics((schema) => ({ zero: schema.make(0) })),
//! )
//! ```
//!
//! and the generated compatibility artifacts agree: `MoneyUSD = number` in
//! `compat/generated-client/promise/generated/types.ts`, `{"type":"number"}`
//! in `compat/openapi/openapi.json`.
//!
//! Three consequences are read directly off that, and none of them is a choice
//! this lane made:
//!
//! 1. **`Schema.Finite` refuses NaN and both infinities.** So does
//!    [`MoneyUsd::new`]. This is the whole of the contract's validation.
//! 2. **The brand carries no sign constraint.** `Schema.Finite` is not
//!    `Schema.NonNegative`, so a negative amount is a value the oracle
//!    accepts. This type accepts it too — see the sign note below.
//! 3. **The representation is a JS `number`, i.e. an IEEE-754 double.** A
//!    decimal type would be better product engineering and would NOT
//!    round-trip what the oracle emits; the legacy column is `cost REAL`.
//!    Faithfulness wins for a compatibility DTO.
//!
//! # The sign decision, and why it differs from the token counters
//!
//! [`MoneyUsd`] accepts a negative amount; [`crate::TokenCount`] refuses one.
//! That asymmetry is derived, not arbitrary:
//!
//! * Nothing in the campaign's law types money as non-negative, and a credit,
//!   refund or correcting adjustment is representable in the frozen contract.
//!   Narrowing here would invent a value the oracle accepts and we refuse.
//! * A token counter, by contrast, has a ratified non-negative owner already
//!   in this crate — `UsageField(Option<u64>)` and `AttemptSpend(u64)` — and
//!   sits beside `EventSeq` ("non-negative sequence") and `ContentOrdinal`
//!   ("non-negative ordinal"). A negative count has no admitted producer.
//!
//! # Negative zero
//!
//! `-0.0` is normalised to `0.0` at construction. In the oracle `-0 === 0` and
//! `JSON.stringify(-0)` is `"0"`, so an un-normalised `-0.0` would be a Rust
//! value that compares and serialises differently from the JavaScript one it
//! claims to be compatible with, while `PartialEq` still called them equal.

use std::fmt;

/// Which non-finite value was refused. Kept distinct rather than collapsed to
/// one "not finite" case: a NaN cost and an infinite cost come from different
/// upstream defects, and a receipt that cannot tell them apart cannot say
/// which one it saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NonFinite {
    /// The value was NaN.
    Nan,
    /// The value was `+∞`.
    PositiveInfinity,
    /// The value was `-∞`.
    NegativeInfinity,
}

impl NonFinite {
    /// Classify a value the contract refuses, or `None` when it is finite.
    /// Shared with [`crate::usage_compat`] so both halves of the
    /// compatibility vocabulary apply ONE reading of `Schema.Finite`.
    pub(crate) fn classify(value: f64) -> Option<Self> {
        if value.is_nan() {
            Some(NonFinite::Nan)
        } else if value == f64::INFINITY {
            Some(NonFinite::PositiveInfinity)
        } else if value == f64::NEG_INFINITY {
            Some(NonFinite::NegativeInfinity)
        } else {
            None
        }
    }

    /// The label used in refusal messages and in the acceptance corpus, so the
    /// corpus and the code cannot drift into two vocabularies.
    pub fn label(self) -> &'static str {
        match self {
            NonFinite::Nan => "nan",
            NonFinite::PositiveInfinity => "positive-infinity",
            NonFinite::NegativeInfinity => "negative-infinity",
        }
    }
}

/// Why a monetary amount was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MoneyError {
    /// The frozen contract is `Schema.Finite`; this value is not finite.
    NotFinite(NonFinite),
    /// A sum of two finite amounts left the finite range. Refused rather than
    /// saturated: a silently clamped total is a wrong number that looks right.
    SumNotFinite(NonFinite),
}

impl fmt::Display for MoneyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MoneyError::NotFinite(kind) => {
                write!(f, "Money.USD is Schema.Finite and refuses {}", kind.label())
            }
            MoneyError::SumNotFinite(kind) => write!(
                f,
                "sum of two finite Money.USD amounts reached {}; refused rather than saturated",
                kind.label()
            ),
        }
    }
}

impl std::error::Error for MoneyError {}

/// A `Money.USD`-compatible amount: a finite IEEE-754 double, exactly what the
/// frozen brand admits.
///
/// `Eq`/`Hash` are deliberately NOT implemented. The inner value is a float
/// and the contract's own type is a float; pretending to a total equality this
/// representation does not have would be a lie in the type system.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct MoneyUsd(f64);

impl MoneyUsd {
    /// The frozen `Money.USD.zero` static.
    pub const ZERO: MoneyUsd = MoneyUsd(0.0);

    /// Construct from a contract number, refusing exactly what
    /// `Schema.Finite` refuses. `-0.0` normalises to `0.0`.
    pub fn new(amount: f64) -> Result<Self, MoneyError> {
        match NonFinite::classify(amount) {
            Some(kind) => Err(MoneyError::NotFinite(kind)),
            // `-0.0 == 0.0` is true, so this arm also catches negative zero.
            None if amount == 0.0 => Ok(MoneyUsd(0.0)),
            None => Ok(MoneyUsd(amount)),
        }
    }

    /// Construct from the legacy `session.cost REAL` column. Separate from
    /// [`MoneyUsd::new`] so a receipt can say which source a value came in
    /// through, and so the legacy edge has one named entry point rather than
    /// being spelled out at each call site. Keep the call here direct: the
    /// legacy edge has its own sign and finite witnesses, and its resurrection
    /// mutants replace this exact call rather than relying on witnesses for a
    /// different constructor.
    pub fn from_legacy_column(amount: f64) -> Result<Self, MoneyError> {
        Self::new(amount)
    }

    /// The amount as the contract number. Total: every constructed value is
    /// finite, so this cannot produce something the contract refuses.
    pub fn amount(self) -> f64 {
        self.0
    }

    /// Add two amounts, refusing a sum that leaves the finite range.
    ///
    /// There is deliberately no `impl Add`: `+` would have to either panic or
    /// return a non-finite value, and both are worse than making the caller
    /// see the refusal.
    pub fn checked_add(self, other: MoneyUsd) -> Result<Self, MoneyError> {
        let sum = self.0 + other.0;
        match NonFinite::classify(sum) {
            Some(kind) => Err(MoneyError::SumNotFinite(kind)),
            None => Self::new(sum),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MoneyError, MoneyUsd, NonFinite};

    #[test]
    fn the_contract_refuses_every_non_finite_amount_and_says_which() {
        assert_eq!(
            MoneyUsd::new(f64::NAN),
            Err(MoneyError::NotFinite(NonFinite::Nan)),
            "Money.USD is Schema.Finite: NaN must be refused AS NaN"
        );
        assert_eq!(
            MoneyUsd::new(f64::INFINITY),
            Err(MoneyError::NotFinite(NonFinite::PositiveInfinity)),
            "Money.USD is Schema.Finite: +inf must be refused AS positive-infinity"
        );
        assert_eq!(
            MoneyUsd::new(f64::NEG_INFINITY),
            Err(MoneyError::NotFinite(NonFinite::NegativeInfinity)),
            "Money.USD is Schema.Finite: -inf must be refused AS negative-infinity"
        );
    }

    #[test]
    fn a_negative_amount_is_accepted_because_the_frozen_brand_has_no_sign_constraint() {
        let credit = MoneyUsd::new(-1.5);
        assert_eq!(
            credit.map(MoneyUsd::amount),
            Ok(-1.5),
            "Schema.Finite is not Schema.NonNegative; refusing a negative cost would \
             invent a value the oracle accepts and this vocabulary rejects"
        );
    }

    #[test]
    fn negative_zero_normalises_so_the_rust_value_matches_the_javascript_one() {
        let normalised = MoneyUsd::new(-0.0);
        let bits = normalised.map(|m| m.amount().to_bits());
        assert_eq!(
            bits,
            Ok(0.0f64.to_bits()),
            "in the oracle -0 === 0 and JSON.stringify(-0) is \"0\"; an un-normalised \
             -0.0 would serialise differently from the value it claims to equal"
        );
    }

    #[test]
    fn the_zero_static_matches_the_frozen_zero() {
        assert_eq!(MoneyUsd::ZERO.amount().to_bits(), 0.0f64.to_bits());
        assert_eq!(MoneyUsd::new(0.0), Ok(MoneyUsd::ZERO));
    }

    #[test]
    fn the_largest_observed_legacy_cost_survives_the_legacy_edge_unchanged() {
        // 26.3144082 is the maximum `session.cost` in the reconstruction
        // evidence clone (R4D-2G preflight, 305 sessions). A compatibility
        // type that altered a real observed value would be the defect.
        let observed = 26.3144082_f64;
        assert_eq!(
            MoneyUsd::from_legacy_column(observed).map(MoneyUsd::amount),
            Ok(observed)
        );
    }

    #[test]
    fn the_legacy_edge_preserves_a_negative_amounts_sign() {
        let credit = -1.5_f64;
        assert_eq!(
            MoneyUsd::from_legacy_column(credit).map(MoneyUsd::amount),
            Ok(credit),
            "the legacy edge must preserve the negative sign; taking an absolute value \
             invents a debit where the frozen Money.USD brand admitted a credit"
        );
    }

    #[test]
    fn the_legacy_edge_refuses_every_non_finite_amount_and_says_which() {
        assert_eq!(
            MoneyUsd::from_legacy_column(f64::NAN),
            Err(MoneyError::NotFinite(NonFinite::Nan)),
            "the public legacy edge must itself refuse NaN AS NaN; a witness only at \
             MoneyUsd::new would not prove this entry point"
        );
        assert_eq!(
            MoneyUsd::from_legacy_column(f64::INFINITY),
            Err(MoneyError::NotFinite(NonFinite::PositiveInfinity)),
            "the public legacy edge must itself refuse +inf AS positive-infinity"
        );
        assert_eq!(
            MoneyUsd::from_legacy_column(f64::NEG_INFINITY),
            Err(MoneyError::NotFinite(NonFinite::NegativeInfinity)),
            "the public legacy edge must itself refuse -inf AS negative-infinity"
        );
    }

    #[test]
    fn a_sum_that_leaves_the_finite_range_is_refused_rather_than_saturated() {
        let big = match MoneyUsd::new(f64::MAX) {
            Ok(value) => value,
            Err(error) => panic!("f64::MAX is finite and must construct: {error}"),
        };
        assert_eq!(
            big.checked_add(big),
            Err(MoneyError::SumNotFinite(NonFinite::PositiveInfinity)),
            "an overflowing total must refuse; f64::MAX saturated to +inf would be a \
             wrong number wearing a valid type"
        );
    }

    #[test]
    fn a_finite_sum_is_returned() {
        let a = match MoneyUsd::new(1.25) {
            Ok(value) => value,
            Err(error) => panic!("1.25 is finite: {error}"),
        };
        let b = match MoneyUsd::new(2.5) {
            Ok(value) => value,
            Err(error) => panic!("2.5 is finite: {error}"),
        };
        assert_eq!(a.checked_add(b).map(MoneyUsd::amount), Ok(3.75));
    }
}
