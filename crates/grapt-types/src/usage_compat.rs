//! Token-usage compatibility vocabulary — the Rust counterpart of the frozen
//! engine's `TokenUsage.Info`.
//!
//! # This is not a replacement for [`crate::usage`]
//!
//! Two usage vocabularies exist on purpose and they answer different
//! questions:
//!
//! | | [`crate::usage::AttemptUsage`] | [`TokenUsageInfo`] (here) |
//! | --- | --- | --- |
//! | question | what did we *observe* for one attempt | what does the compatibility DTO *say* |
//! | fields | input/output only, each optional | all five present, none optional |
//! | absence | [`crate::UsageField::ABSENT`], never zero | unrepresentable |
//! | phase / provenance | carried | out of scope |
//!
//! The ratified row for `grapt-types::usage` reads "absent is not zero,
//! reported is not estimated, later observations reconcile by field/phase
//! without double charging." Nothing here weakens that. The frozen DTO simply
//! has no way to *say* "absent" — every one of its five members is required —
//! There is deliberately no `AttemptUsage -> TokenUsageInfo` conversion:
//! [`crate::usage::AttemptUsage`] owns input/output only. Reasoning,
//! `cache.read`, and `cache.write` have no upstream `grapt-types` owner yet,
//! so such a conversion would have to synthesize zeros or absence and lie.
//! [`TokenUsageInfo::from_observed`] instead requires five explicit fields from
//! a future source owner and refuses naming any absent leaf. R4D-2G2's bounded
//! legacy projection and R4D-2G3's `session.usage.recorded.1` fold are the
//! expected source seams; this G1 vocabulary invents neither one.
//!
//! # What the frozen contract actually says
//!
//! Oracle `430859db`, `packages/schema/src/token-usage.ts`:
//!
//! ```text
//! export const Info = Schema.Struct({
//!   input: Schema.Finite,
//!   output: Schema.Finite,
//!   reasoning: Schema.Finite,
//!   cache: Schema.Struct({ read: Schema.Finite, write: Schema.Finite }),
//! }).annotate({ identifier: "TokenUsage.Info" })
//! ```
//!
//! `compat/openapi/openapi.json` pins the same shape with
//! `additionalProperties: false` and `required: [input, output, reasoning,
//! cache]`, `cache` requiring `[read, write]`. So the field set is closed and
//! exactly five leaves; [`TokenUsageInfo::FIELD_LABELS`] is that set in
//! contract order, and its length is a compile-time constant, so adding a
//! sixth leaf cannot compile without editing the law in one place.
//!
//! # The three numeric decisions, each derived
//!
//! **Non-finite is refused.** `Schema.Finite` refuses NaN and both
//! infinities. Same rule as [`crate::money`], same reason.
//!
//! **Negative is refused.** The contract's leaf type is `Schema.Finite`, not
//! `Schema.NonNegative`, so this is a deliberate NARROWING and is disclosed as
//! one. It is derived rather than invented: the ratified owner of a token
//! count in this crate is already unsigned (`UsageField(Option<u64>)`,
//! `AttemptSpend(u64)`), it sits beside `EventSeq` ("non-negative sequence")
//! and `ContentOrdinal` ("non-negative ordinal"), and the legacy source is a
//! counter column. No admitted producer can emit a negative count, so the
//! narrowing cannot cause a divergence on any value the engine actually
//! produces. Contrast [`crate::MoneyUsd`], which keeps the sign.
//!
//! **Counts above 2^53 are refused by this lane.** `2^53` is the largest `N`
//! for which *every* integer in `[0, N]` is exactly representable as an
//! IEEE-754 binary64. Some larger integers (for example `2^55`) are exactly
//! representable too, while adjacent ones are not; admitting that sparse set
//! would make an increment cease to be a truthful count operation. This lane
//! therefore chooses one contiguous count domain ending at
//! [`MAX_EXACT_TOKEN_COUNT`]. That is a disclosed narrowing of the frozen
//! `Schema.Finite` leaf, not an oracle property.
//!
//! This ceiling is intentionally distinct from JavaScript's
//! `Number.MAX_SAFE_INTEGER = 2^53 - 1`. `2^53` itself is exactly
//! representable and this lane admits it; it is not a *safe integer* in the JS
//! sense because adding one compares equal to it. Both sides of the lane's
//! chosen ceiling are witnessed.
//!
//! Arithmetic is `checked_` only and re-checks the contract bound, never the
//! `u64` bound alone. There is no `impl Add`, for the reason given in
//! [`crate::money`].

use crate::usage::UsageField;
use std::fmt;

/// This lane's contiguous exact-integer ceiling: the largest `N` for which
/// every integer in `[0, N]` is exactly representable as an IEEE-754 binary64.
///
/// This is `2^53 = 9,007,199,254,740,992`, not JavaScript's
/// `Number.MAX_SAFE_INTEGER` (`2^53 - 1`). Some larger isolated integers are
/// representable, but this compatibility vocabulary deliberately refuses the
/// sparse region so every admitted count remains in one contiguous domain.
pub const MAX_EXACT_TOKEN_COUNT: u64 = 1u64 << 53;

/// Which leaf of `TokenUsage.Info` a refusal is about. Carried so a refusal
/// names the field rather than reporting that "usage" failed — a projection
/// that cannot say which counter was missing cannot be acted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageFieldName {
    Input,
    Output,
    Reasoning,
    CacheRead,
    CacheWrite,
}

impl UsageFieldName {
    /// The contract label, dotted for the nested `cache` members. Shared by
    /// refusal messages, [`TokenUsageInfo::labelled_counts`] and the
    /// acceptance corpus, so those three cannot drift into three spellings.
    pub fn label(self) -> &'static str {
        match self {
            UsageFieldName::Input => "input",
            UsageFieldName::Output => "output",
            UsageFieldName::Reasoning => "reasoning",
            UsageFieldName::CacheRead => "cache.read",
            UsageFieldName::CacheWrite => "cache.write",
        }
    }
}

impl fmt::Display for UsageFieldName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Why a token count was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenCountError {
    /// A contract number that is NaN or infinite.
    NotFinite(crate::money::NonFinite),
    /// A count below zero. See the module note: a deliberate, disclosed
    /// narrowing of the contract's `Schema.Finite` leaf.
    Negative,
    /// A contract number with a fractional part. A token count is a count.
    NotIntegral,
    /// A count above [`MAX_EXACT_TOKEN_COUNT`], which the compatibility DTO's
    /// lane-owned contiguous count domain refuses.
    AboveExactBound { value: u64 },
    /// A finite contract number above the lane bound, refused before the cast.
    /// `value_bits` is the exact IEEE-754 representation supplied at the public
    /// edge; it replaces the held tip's fabricated `2^53 + 1` sentinel.
    ContractNumberAboveExactBound { value_bits: u64 },
    /// A sum that left this lane's contiguous count domain. Refused, never
    /// saturated or admitted merely because one sparse binary64 value exists.
    SumAboveExactBound,
}

impl fmt::Display for TokenCountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenCountError::NotFinite(kind) => write!(
                f,
                "a TokenUsage.Info leaf is Schema.Finite and refuses {}",
                kind.label()
            ),
            TokenCountError::Negative => f.write_str("a token count is unsigned; negative refused"),
            TokenCountError::NotIntegral => {
                f.write_str("a token count is a count; a fractional value is refused")
            }
            TokenCountError::AboveExactBound { value } => write!(
                f,
                "token count {value} exceeds {MAX_EXACT_TOKEN_COUNT}, this lane's \
                 contiguous exact-integer ceiling"
            ),
            TokenCountError::ContractNumberAboveExactBound { value_bits } => write!(
                f,
                "contract number {} (binary64 bits 0x{value_bits:016x}) exceeds \
                 {MAX_EXACT_TOKEN_COUNT}, this lane's contiguous exact-integer ceiling",
                f64::from_bits(*value_bits)
            ),
            TokenCountError::SumAboveExactBound => write!(
                f,
                "a sum of token counts exceeded {MAX_EXACT_TOKEN_COUNT}; refused \
                 rather than saturated or wrapped"
            ),
        }
    }
}

impl std::error::Error for TokenCountError {}

/// One `TokenUsage.Info` leaf: a non-negative count that the compatibility
/// DTO can carry exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TokenCount(u64);

impl TokenCount {
    /// A count of zero. This is an OBSERVED zero — it is never what an absent
    /// measurement becomes. See [`TokenUsageInfo::from_observed`].
    pub const ZERO: TokenCount = TokenCount(0);

    /// Construct from an internal unsigned count, enforcing this lane's
    /// contiguous exact-integer ceiling.
    pub fn new(value: u64) -> Result<Self, TokenCountError> {
        if value > MAX_EXACT_TOKEN_COUNT {
            return Err(TokenCountError::AboveExactBound { value });
        }
        Ok(TokenCount(value))
    }

    /// Construct from a legacy `tokens_* INTEGER` column, which is signed at
    /// the storage layer even though the value is a counter.
    pub fn from_legacy_column(value: i64) -> Result<Self, TokenCountError> {
        if value < 0 {
            return Err(TokenCountError::Negative);
        }
        // Non-negative i64 always fits u64; the contract bound is checked next.
        Self::new(value.unsigned_abs())
    }

    /// Construct from a contract number, applying every rule the DTO implies.
    pub fn from_contract_number(value: f64) -> Result<Self, TokenCountError> {
        if let Some(kind) = crate::money::NonFinite::classify(value) {
            return Err(TokenCountError::NotFinite(kind));
        }
        if value < 0.0 {
            return Err(TokenCountError::Negative);
        }
        if value.fract() != 0.0 {
            return Err(TokenCountError::NotIntegral);
        }
        // Checked before the cast so this public edge returns a distinct,
        // exact refusal containing the supplied binary64 bits. Removing this
        // guard still refuses later through `new`, but loses which f64 crossed
        // the edge; the dedicated UT mutant proves that distinction.
        if value > MAX_EXACT_TOKEN_COUNT as f64 {
            return Err(TokenCountError::ContractNumberAboveExactBound {
                value_bits: value.to_bits(),
            });
        }
        Self::new(value as u64)
    }

    /// The count as an internal unsigned value.
    pub fn value(self) -> u64 {
        self.0
    }

    /// The count as the contract number. Total and lossless inside the lane's
    /// contiguous exact-integer domain.
    pub fn as_contract_number(self) -> f64 {
        self.0 as f64
    }

    /// Add two counts, refusing a total outside this lane's contiguous count
    /// domain. Note this re-checks the LANE bound, not merely `u64` overflow:
    /// sparse larger values may be binary64-representable but are deliberately
    /// outside the admitted domain.
    pub fn checked_add(self, other: TokenCount) -> Result<Self, TokenCountError> {
        let sum = self
            .0
            .checked_add(other.0)
            .ok_or(TokenCountError::SumAboveExactBound)?;
        if sum > MAX_EXACT_TOKEN_COUNT {
            return Err(TokenCountError::SumAboveExactBound);
        }
        Ok(TokenCount(sum))
    }
}

/// Why a `TokenUsage.Info` could not be projected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageCompatError {
    /// The named field was never observed. It is NOT zero, and this crate
    /// does not decide what it should become — that is product semantics.
    FieldAbsent(UsageFieldName),
    /// The named field was observed but its value is not carriable.
    FieldNotCarriable {
        field: UsageFieldName,
        error: TokenCountError,
    },
}

impl fmt::Display for UsageCompatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UsageCompatError::FieldAbsent(field) => write!(
                f,
                "TokenUsage.Info requires {field}, which was never observed; an \
                 unobserved counter is not a zero counter and this projection will \
                 not invent one"
            ),
            UsageCompatError::FieldNotCarriable { field, error } => {
                write!(f, "TokenUsage.Info field {field} is not carriable: {error}")
            }
        }
    }
}

impl std::error::Error for UsageCompatError {}

/// The nested `cache` member of `TokenUsage.Info`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TokenCacheUsage {
    pub read: TokenCount,
    pub write: TokenCount,
}

/// A `TokenUsage.Info`-compatible value: five counts, all present, in the
/// contract's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TokenUsageInfo {
    pub input: TokenCount,
    pub output: TokenCount,
    pub reasoning: TokenCount,
    pub cache: TokenCacheUsage,
}

impl TokenUsageInfo {
    /// The number of leaves in the frozen contract. A compile-time constant so
    /// the label list, the projection and every array below are bound to one
    /// another: changing the field set fails to compile rather than leaving a
    /// stale label list agreeing with itself.
    pub const FIELD_COUNT: usize = 5;

    /// Every leaf label in EXACT contract order. Order is part of the
    /// contract, not a presentation detail: it is the order the corpus pins
    /// and the order a canonical rendering must use.
    pub const FIELD_LABELS: [&'static str; Self::FIELD_COUNT] =
        ["input", "output", "reasoning", "cache.read", "cache.write"];

    /// Every leaf name in EXACT contract order, as the typed enum.
    pub const FIELD_NAMES: [UsageFieldName; Self::FIELD_COUNT] = [
        UsageFieldName::Input,
        UsageFieldName::Output,
        UsageFieldName::Reasoning,
        UsageFieldName::CacheRead,
        UsageFieldName::CacheWrite,
    ];

    /// Construct from five explicitly sourced optional observations.
    ///
    /// Fallible by construction: the frozen DTO requires all five leaves and
    /// [`UsageField::ABSENT`] means "not observed". A missing field is
    /// refused, NAMING the field — it is never defaulted to zero, because the
    /// ratified `grapt-types::usage` row says absent is not zero, and because
    /// the reconstruction evidence shows the two are distinguishable in
    /// practice (88 sessions carry a real cost with no usage record at all).
    ///
    /// This is NOT a conversion from [`crate::usage::AttemptUsage`], which
    /// currently owns only input/output. No `grapt-types` owner supplies
    /// reasoning or either cache leaf yet. The expected sources are the R4D-2G2
    /// bounded legacy projection and R4D-2G3's `session.usage.recorded.1` fold;
    /// until one of those owners calls this seam with five honest observations,
    /// this module synthesizes neither zeros nor an invented conversion.
    ///
    /// What a caller does with a refusal is not decided here.
    pub fn from_observed(
        input: UsageField,
        output: UsageField,
        reasoning: UsageField,
        cache_read: UsageField,
        cache_write: UsageField,
    ) -> Result<Self, UsageCompatError> {
        let leaf = |field: UsageFieldName, observed: UsageField| {
            let value = observed
                .value()
                .ok_or(UsageCompatError::FieldAbsent(field))?;
            TokenCount::new(value)
                .map_err(|error| UsageCompatError::FieldNotCarriable { field, error })
        };
        Ok(TokenUsageInfo {
            input: leaf(UsageFieldName::Input, input)?,
            output: leaf(UsageFieldName::Output, output)?,
            reasoning: leaf(UsageFieldName::Reasoning, reasoning)?,
            cache: TokenCacheUsage {
                read: leaf(UsageFieldName::CacheRead, cache_read)?,
                write: leaf(UsageFieldName::CacheWrite, cache_write)?,
            },
        })
    }

    /// Project from the five legacy `tokens_* INTEGER` columns, in the order
    /// the legacy row declares them.
    pub fn from_legacy_columns(
        input: i64,
        output: i64,
        reasoning: i64,
        cache_read: i64,
        cache_write: i64,
    ) -> Result<Self, UsageCompatError> {
        let leaf = |field: UsageFieldName, value: i64| {
            TokenCount::from_legacy_column(value)
                .map_err(|error| UsageCompatError::FieldNotCarriable { field, error })
        };
        Ok(TokenUsageInfo {
            input: leaf(UsageFieldName::Input, input)?,
            output: leaf(UsageFieldName::Output, output)?,
            reasoning: leaf(UsageFieldName::Reasoning, reasoning)?,
            cache: TokenCacheUsage {
                read: leaf(UsageFieldName::CacheRead, cache_read)?,
                write: leaf(UsageFieldName::CacheWrite, cache_write)?,
            },
        })
    }

    /// Every leaf as `(label, count)` in EXACT contract order.
    ///
    /// Derived from the fields themselves, so it cannot agree with a stale
    /// copy of the values; the ORDER is the law this carries, and the labels
    /// come from [`UsageFieldName::label`] rather than a second spelling.
    pub fn labelled_counts(&self) -> [(&'static str, u64); Self::FIELD_COUNT] {
        [
            (UsageFieldName::Input.label(), self.input.value()),
            (UsageFieldName::Output.label(), self.output.value()),
            (UsageFieldName::Reasoning.label(), self.reasoning.value()),
            (UsageFieldName::CacheRead.label(), self.cache.read.value()),
            (UsageFieldName::CacheWrite.label(), self.cache.write.value()),
        ]
    }

    /// The total token count across every leaf, refusing a total outside this
    /// lane's contiguous count domain.
    pub fn checked_total(&self) -> Result<TokenCount, TokenCountError> {
        let mut total = TokenCount::ZERO;
        for (_, count) in self.labelled_counts() {
            total = total.checked_add(TokenCount::new(count)?)?;
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        TokenCacheUsage, TokenCount, TokenCountError, TokenUsageInfo, UsageCompatError,
        UsageFieldName, MAX_EXACT_TOKEN_COUNT,
    };
    use crate::money::NonFinite;
    use crate::usage::UsageField;

    fn count(value: u64) -> TokenCount {
        match TokenCount::new(value) {
            Ok(count) => count,
            Err(error) => panic!("{value} is within the contract bound: {error}"),
        }
    }

    /// Five DISTINCT values, so an order swap between any two leaves is
    /// detectable. Equal values would let a transposition pass.
    fn distinct_usage() -> TokenUsageInfo {
        TokenUsageInfo {
            input: count(11),
            output: count(22),
            reasoning: count(33),
            cache: TokenCacheUsage {
                read: count(44),
                write: count(55),
            },
        }
    }

    #[test]
    fn an_absent_field_refuses_and_names_the_field_rather_than_becoming_zero() {
        let refusal = TokenUsageInfo::from_observed(
            UsageField::reported(count(10)),
            UsageField::reported(count(20)),
            UsageField::ABSENT,
            UsageField::reported(count(40)),
            UsageField::reported(count(50)),
        );
        assert_eq!(
            refusal,
            Err(UsageCompatError::FieldAbsent(UsageFieldName::Reasoning)),
            "an unobserved counter must refuse NAMING reasoning; defaulting it to zero \
             is the exact failure the ratified usage row forbids"
        );
    }

    #[test]
    fn each_of_the_five_leaves_can_be_the_absent_one() {
        // A refusal that only fires for one field would leave four silent
        // defaults behind it.
        let present = UsageField::reported(count(1));
        let cases = [
            (0usize, UsageFieldName::Input),
            (1, UsageFieldName::Output),
            (2, UsageFieldName::Reasoning),
            (3, UsageFieldName::CacheRead),
            (4, UsageFieldName::CacheWrite),
        ];
        for (index, expected) in cases {
            let mut fields: [UsageField; TokenUsageInfo::FIELD_COUNT] =
                std::array::from_fn(|_| present.clone());
            fields[index] = UsageField::ABSENT;
            let [input, output, reasoning, cache_read, cache_write] = fields;
            let refusal =
                TokenUsageInfo::from_observed(input, output, reasoning, cache_read, cache_write);
            assert_eq!(
                refusal,
                Err(UsageCompatError::FieldAbsent(expected)),
                "absence at leaf {} must refuse naming {expected}",
                expected.label()
            );
        }
    }

    #[test]
    fn an_observed_zero_is_carried_and_is_not_an_absence() {
        let all_zero = TokenUsageInfo::from_observed(
            UsageField::reported(TokenCount::ZERO),
            UsageField::reported(TokenCount::ZERO),
            UsageField::reported(TokenCount::ZERO),
            UsageField::reported(TokenCount::ZERO),
            UsageField::reported(TokenCount::ZERO),
        );
        assert_eq!(
            all_zero.map(|usage| usage.input.value()),
            Ok(0),
            "an OBSERVED zero is a fact and must project; only an ABSENT field refuses"
        );
    }

    #[test]
    fn the_leaf_order_and_labels_are_the_contract_order() {
        assert_eq!(
            distinct_usage().labelled_counts(),
            [
                ("input", 11),
                ("output", 22),
                ("reasoning", 33),
                ("cache.read", 44),
                ("cache.write", 55),
            ],
            "TokenUsage.Info leaf order/labels are contract, not presentation; the five \
             values are distinct so a transposition cannot pass"
        );
        assert_eq!(TokenUsageInfo::FIELD_LABELS.len(), 5);
        assert_eq!(
            TokenUsageInfo::FIELD_NAMES.map(UsageFieldName::label),
            TokenUsageInfo::FIELD_LABELS,
            "the typed leaf list and the label list must be one law, not two"
        );
    }

    #[test]
    fn a_negative_legacy_column_is_refused_naming_its_field() {
        assert_eq!(
            TokenUsageInfo::from_legacy_columns(1, -1, 3, 4, 5),
            Err(UsageCompatError::FieldNotCarriable {
                field: UsageFieldName::Output,
                error: TokenCountError::Negative,
            }),
            "a negative counter has no admitted producer and is refused at the legacy edge"
        );
        assert_eq!(
            TokenCount::from_legacy_column(i64::MIN),
            Err(TokenCountError::Negative),
            "the most negative i64 must refuse as Negative, not wrap through unsigned_abs"
        );
    }

    #[test]
    fn the_contiguous_exactness_ceiling_admits_two_to_the_fifty_three_and_refuses_one_more() {
        assert_eq!(MAX_EXACT_TOKEN_COUNT, 9_007_199_254_740_992);
        assert_eq!(
            TokenCount::new(MAX_EXACT_TOKEN_COUNT).map(TokenCount::value),
            Ok(MAX_EXACT_TOKEN_COUNT),
            "2^53 itself is exactly representable and this lane admits it; this is \
             distinct from JavaScript MAX_SAFE_INTEGER = 2^53 - 1"
        );
        assert_eq!(
            TokenCount::new(MAX_EXACT_TOKEN_COUNT + 1),
            Err(TokenCountError::AboveExactBound {
                value: MAX_EXACT_TOKEN_COUNT + 1
            }),
            "one above the bound is not exactly representable and must be REFUSED"
        );
    }

    #[test]
    fn a_contract_number_must_be_finite_non_negative_and_whole() {
        assert_eq!(
            TokenCount::from_contract_number(f64::NAN),
            Err(TokenCountError::NotFinite(NonFinite::Nan)),
            "a token leaf is Schema.Finite too: NaN must be refused AS NaN"
        );
        assert_eq!(
            TokenCount::from_contract_number(f64::INFINITY),
            Err(TokenCountError::NotFinite(NonFinite::PositiveInfinity)),
            "a token leaf must refuse +inf AS positive-infinity"
        );
        assert_eq!(
            TokenCount::from_contract_number(-1.0),
            Err(TokenCountError::Negative),
            "a negative contract number must refuse BEFORE the cast; `-1.0 as u64` \
             saturates to 0, which would turn this refusal into an observed zero"
        );
        assert_eq!(
            TokenCount::from_contract_number(1.5),
            Err(TokenCountError::NotIntegral),
            "a fractional token count is not a count"
        );
        assert_eq!(
            TokenCount::from_contract_number(7.0).map(TokenCount::value),
            Ok(7),
            "a whole, finite, non-negative contract number must be ACCEPTED, or the \
             four refusals above would be satisfied by a constructor that refuses \
             everything"
        );
    }

    #[test]
    fn an_above_bound_contract_number_refusal_preserves_the_supplied_f64_bits() {
        // 2^55 is itself exactly representable; the lane still refuses it
        // because it is beyond the contiguous all-integers ceiling. The exact
        // bits make the pre-cast refusal distinct and observable rather than
        // fabricating `2^53 + 1` as the held candidate did.
        let above = (MAX_EXACT_TOKEN_COUNT as f64) * 4.0;
        assert_eq!(
            TokenCount::from_contract_number(above),
            Err(TokenCountError::ContractNumberAboveExactBound {
                value_bits: above.to_bits(),
            }),
            "the pre-cast guard must refuse with the exact supplied binary64 bits; \
             falling through to TokenCount::new loses the public-edge representation"
        );
        let rendered = TokenCountError::ContractNumberAboveExactBound {
            value_bits: above.to_bits(),
        }
        .to_string();
        assert!(
            rendered.contains("0x4360000000000000"),
            "the exact supplied representation must be observable in the refusal: {rendered}"
        );
    }

    #[test]
    fn a_sum_beyond_the_contract_bound_refuses_without_overflowing_u64() {
        let half = count(MAX_EXACT_TOKEN_COUNT / 2);
        assert_eq!(
            half.checked_add(half).map(TokenCount::value),
            Ok(MAX_EXACT_TOKEN_COUNT),
            "a sum that lands exactly on the bound is representable and must be admitted"
        );
        let one = count(1);
        assert_eq!(
            count(MAX_EXACT_TOKEN_COUNT).checked_add(one),
            Err(TokenCountError::SumAboveExactBound),
            "a sum one past the bound must refuse; the u64 has not overflowed, so a \
             u64-only overflow check would miss this entirely"
        );
    }

    #[test]
    fn the_total_refuses_rather_than_reporting_a_wrong_number() {
        assert_eq!(
            distinct_usage().checked_total().map(TokenCount::value),
            Ok(11 + 22 + 33 + 44 + 55)
        );
        let near_bound = TokenUsageInfo {
            input: count(MAX_EXACT_TOKEN_COUNT),
            output: count(1),
            reasoning: TokenCount::ZERO,
            cache: TokenCacheUsage {
                read: TokenCount::ZERO,
                write: TokenCount::ZERO,
            },
        };
        assert_eq!(
            near_bound.checked_total(),
            Err(TokenCountError::SumAboveExactBound),
            "a total beyond the bound must refuse, not saturate to the bound"
        );
    }

    #[test]
    fn a_carriable_count_round_trips_through_the_contract_number() {
        for value in [0, 1, 2047, MAX_EXACT_TOKEN_COUNT] {
            let carried = count(value).as_contract_number();
            assert_eq!(
                TokenCount::from_contract_number(carried).map(TokenCount::value),
                Ok(value),
                "every count within the bound must survive the DTO round trip exactly"
            );
        }
    }
}
