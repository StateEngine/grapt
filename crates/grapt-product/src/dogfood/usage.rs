//! Honest provider-usage projection for the retained compatibility surface.
//!
//! Provider fields remain optional in durable truth. The public usage DTO
//! requires numeric fields, so an incomplete live observation is withheld and
//! the message surface omits its optional outer `tokens` field; neither path
//! invents observed zero. The exact inclusive 2^53 ceiling keeps every public f64
//! token integer exact and is enforced before conversion and aggregate addition.

use grapt_api::u3a::{FiniteF64, MoneyUSDDto, TokenUsageInfoDto, TokenUsageInfoDtoCache};
use grapt_provider::TokenUsage;
use grapt_schema::{Optional, Required};
use grapt_types::{MoneyUsd, RecordedAttemptUsage, TokenCount};

use super::execution::ExecutionRefusal;

#[cfg(test)]
pub(super) const PROVIDER_USAGE_TOKEN_CEILING: u64 = grapt_types::MAX_EXACT_TOKEN_COUNT;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct ReconciledUsage {
    input: Option<u64>,
    output: Option<u64>,
    reasoning: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
}

impl ReconciledUsage {
    /// Contribution to a sum of *recorded* counters. Absence contributes
    /// nothing to that field's sum; it does not discard other reported fields
    /// or change the canonical observation into a report of zero.
    #[cfg(test)]
    pub(super) fn recorded_contribution(self) -> CompleteUsage {
        CompleteUsage {
            input: self.input.unwrap_or(0),
            output: self.output.unwrap_or(0),
            reasoning: self.reasoning.unwrap_or(0),
            cache_read: self.cache_read.unwrap_or(0),
            cache_write: self.cache_write.unwrap_or(0),
        }
    }
    #[cfg(test)]
    pub(super) fn complete(self, context: &str) -> Result<CompleteUsage, String> {
        Ok(CompleteUsage {
            input: required_usage(self.input, context, "input")?,
            output: required_usage(self.output, context, "output")?,
            reasoning: required_usage(self.reasoning, context, "reasoning")?,
            cache_read: required_usage(self.cache_read, context, "cache-read-input")?,
            cache_write: required_usage(self.cache_write, context, "cache-write-input")?,
        })
    }

    /// Project only an observation that can satisfy the compatibility DTO's
    /// required inner coordinates. Assistant-message `tokens` is optional, so
    /// absence here means omit that outer field rather than fail the message.
    pub(super) fn complete_if_observed(self) -> Option<CompleteUsage> {
        Some(CompleteUsage {
            input: self.input?,
            output: self.output?,
            reasoning: self.reasoning?,
            cache_read: self.cache_read?,
            cache_write: self.cache_write?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CompleteUsage {
    pub(super) input: u64,
    pub(super) output: u64,
    pub(super) reasoning: u64,
    pub(super) cache_read: u64,
    pub(super) cache_write: u64,
}

impl CompleteUsage {
    pub(super) const ZERO: Self = Self {
        input: 0,
        output: 0,
        reasoning: 0,
        cache_read: 0,
        cache_write: 0,
    };

    pub(super) fn checked_add(self, next: Self) -> Result<Self, String> {
        Ok(Self {
            input: checked_usage_add(self.input, next.input, "input")?,
            output: checked_usage_add(self.output, next.output, "output")?,
            reasoning: checked_usage_add(self.reasoning, next.reasoning, "reasoning")?,
            cache_read: checked_usage_add(self.cache_read, next.cache_read, "cache-read-input")?,
            cache_write: checked_usage_add(
                self.cache_write,
                next.cache_write,
                "cache-write-input",
            )?,
        })
    }
}

pub(super) fn reconciled_usage(
    observations: &[RecordedAttemptUsage],
) -> Result<ReconciledUsage, String> {
    let usage = grapt_types::usage::reconcile_recorded_usage(observations)
        .map_err(|error| error.to_string())?;
    Ok(reconciled_counts(&usage))
}

/// Recorded equations prove which absent coordinates do not participate in a
/// compatibility total. A stamped observation carries the protocol family the
/// route observed, and the family is authoritative over the equation
/// inferences; an unstamped observation keeps the pre-field inferences.
/// Canonical observations remain absent in every case, and usage never fails
/// the turn: the worst outcome is an unavailable total.
pub(super) fn aggregate_usage(
    observations: &[RecordedAttemptUsage],
) -> Result<Option<CompleteUsage>, String> {
    use grapt_types::usage::{UsageDerivation, UsageProvenance};
    let usage = grapt_types::usage::reconcile_recorded_usage(observations)
        .map_err(|error| error.to_string())?;
    let mut counts = reconciled_counts(&usage);
    match stamped_family(observations) {
        StampedFamily::Unstamped => {
            // Legacy records predate the protocol stamp: keep the recorded
            // derivation inferences exactly as written before the field.
            if matches!(
                usage.input_total().map(|field| field.provenance()),
                Some(UsageProvenance::ProviderDerived {
                    rule: UsageDerivation::InclusiveInputFromPartition
                })
            ) && matches!(
                usage.input_fresh().map(|field| field.provenance()),
                Some(UsageProvenance::ProviderReported)
            ) {
                // Fresh-input partition protocols include reasoning in
                // output without reporting a separate reasoning coordinate.
                counts.reasoning.get_or_insert(0);
            }
            if matches!(
                usage.output().map(|field| field.provenance()),
                Some(UsageProvenance::ProviderDerived {
                    rule: UsageDerivation::OutputFromTotalLessInput
                })
            ) {
                // The total-less-input output equation has no cache-write
                // coordinate.
                counts.cache_write.get_or_insert(0);
            }
        }
        StampedFamily::Conflicting => {
            // Two different families in one attempt corrupt the family; the
            // total is unavailable rather than wrong, and never an error.
            return Ok(None);
        }
        StampedFamily::Family(family) => fill_stamped_family(&mut counts, family, &usage),
    }
    Ok(counts.complete_if_observed())
}

/// The protocol family an attempt's observations agree on. Only stamped
/// observations vote; legacy rows never vote and never conflict.
fn stamped_family(observations: &[RecordedAttemptUsage]) -> StampedFamily {
    let mut family: Option<grapt_types::UsageProtocolFamily> = None;
    for observation in observations {
        let RecordedAttemptUsage::V2(usage) = observation else {
            continue;
        };
        let Some(stamp) = usage.protocol() else {
            continue;
        };
        family = Some(match family {
            None => stamp,
            Some(held) if held == stamp => held,
            Some(_) => return StampedFamily::Conflicting,
        });
    }
    match family {
        Some(family) => StampedFamily::Family(family),
        None => StampedFamily::Unstamped,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StampedFamily {
    /// No observation carries a stamp: the pre-field inference path.
    Unstamped,
    /// The attempt's stamped observations agree on one family.
    Family(grapt_types::UsageProtocolFamily),
    /// The attempt's observations carry two different families: that
    /// attempt's total is unavailable.
    Conflicting,
}

/// Fill the coordinates a stamped family proves from the protocol's
/// guarantees. The fills touch only the compatibility total: canonical
/// leaves remain absent, so the observed DTO still shows absent as absent.
///
/// A fill exists only where a recorded guarantee proves the omitted
/// coordinate's contribution is zero; every other absence stays unknown and
/// the total stays unavailable rather than invented.
fn fill_stamped_family(
    counts: &mut ReconciledUsage,
    family: grapt_types::UsageProtocolFamily,
    usage: &grapt_types::ReconciledUsage,
) {
    use grapt_types::usage::{UsageDerivation, UsageProvenance};
    match family {
        // Bedrock omits the optional cache counters when caching did not
        // contribute, and the Anthropic wire carries both counters with zero
        // when caching did not contribute: an omitted counter is a zero
        // contribution.
        grapt_types::UsageProtocolFamily::FreshInputPartition => {
            counts.cache_read.get_or_insert(0);
            counts.cache_write.get_or_insert(0);
            // The stamped row keeps the recorded partition inference the
            // unstamped path applies under the same evidence: the partition
            // equation is recorded and the fresh input is reported, so
            // reasoning is folded into output with no separate coordinate.
            if matches!(
                usage.input_total().map(|field| field.provenance()),
                Some(UsageProvenance::ProviderDerived {
                    rule: UsageDerivation::InclusiveInputFromPartition
                })
            ) && matches!(
                usage.input_fresh().map(|field| field.provenance()),
                Some(UsageProvenance::ProviderReported)
            ) {
                counts.reasoning.get_or_insert(0);
            }
        }
        // The Cohere wire is a closed two-counter accounting: it has no
        // cache-write coordinate and no reasoning coordinate at all, and it
        // reports the read counter only when the cache contributed. Nothing
        // is omitted that could hide a contribution, so the absent counters
        // are protocol zeros: a statement about the wire's accounting, not
        // about the model; the canonical reasoning leaf stays absent.
        grapt_types::UsageProtocolFamily::InclusiveInputNoCache => {
            counts.cache_read.get_or_insert(0);
            counts.cache_write.get_or_insert(0);
            counts.reasoning.get_or_insert(0);
        }
        // An omitted cache counter does not prove a zero cache, and the
        // family's routes (including arbitrary compatible ones) may omit a
        // reasoning counter for reasons other than "none occurred": the
        // absent coordinate stays unknown. The one provable zero is the
        // write coordinate under the total-less-input output equation,
        // whose remainder leaves no write partition.
        grapt_types::UsageProtocolFamily::InclusiveInput => {
            if matches!(
                usage.output().map(|field| field.provenance()),
                Some(UsageProvenance::ProviderDerived {
                    rule: UsageDerivation::OutputFromTotalLessInput
                })
            ) {
                counts.cache_write.get_or_insert(0);
            }
        }
    }
}

fn reconciled_counts(usage: &grapt_types::ReconciledUsage) -> ReconciledUsage {
    ReconciledUsage {
        // Public TokenUsage.Info `input` is fresh input. A v1-only ambiguous
        // input observation therefore leaves this absent rather than being
        // relabeled as either fresh or inclusive.
        input: usage.input_fresh().map(|field| field.count().value()),
        output: usage.output().map(|field| field.count().value()),
        reasoning: usage.reasoning().map(|field| field.count().value()),
        cache_read: usage.cache_read().map(|field| field.count().value()),
        cache_write: usage.cache_write().map(|field| field.count().value()),
    }
}

pub(super) fn observed_usage_dto(
    observations: &[RecordedAttemptUsage],
) -> Result<grapt_api::u3a::TokenUsageObservationDto, String> {
    use grapt_api::u3a::{SafeTokenCount, TokenUsageObservationDto};
    let usage = grapt_types::usage::reconcile_recorded_usage(observations)
        .map_err(|error| error.to_string())?;
    let count = |field: Option<&grapt_types::usage::ReconciledUsageField>| {
        field
            .map(|field| SafeTokenCount::try_new(field.count().value()))
            .transpose()
            .map(|value| value.map_or_else(Optional::absent, Optional::present))
            .map_err(str::to_owned)
    };
    Ok(TokenUsageObservationDto::new(
        count(usage.input_total())?,
        count(usage.input_fresh())?,
        count(usage.output())?,
        count(usage.reasoning())?,
        count(usage.cache_read())?,
        count(usage.cache_write())?,
        count(usage.total())?,
    ))
}

/// Project accounting independently of durable message content. An observation
/// outside the public count bound remains unknown rather than hiding the row.
pub(super) fn history_usage(
    observations: &[RecordedAttemptUsage],
) -> Result<(Option<CompleteUsage>, String), String> {
    let Ok(observed) = observed_usage_dto(observations) else {
        return Ok((None, "{}".to_owned()));
    };
    let complete = reconciled_usage(observations)
        .ok()
        .and_then(ReconciledUsage::complete_if_observed);
    let observed = serde_json::to_string(&observed).map_err(|error| error.to_string())?;
    Ok((complete, observed))
}

pub(super) fn usage_dto(usage: TokenUsage) -> Result<Option<TokenUsageInfoDto>, ExecutionRefusal> {
    for (field, value) in [
        ("input-total", usage.input_total().value()),
        ("input-fresh", usage.input_fresh().value()),
        ("output", usage.output().value()),
        ("reasoning", usage.reasoning().value()),
        ("cache-read-input", usage.cache_read().value()),
        ("cache-write-input", usage.cache_write().value()),
        ("total", usage.total().value()),
    ] {
        if let Some(value) = value {
            bounded_usage_value(value, field)?;
        }
    }
    let (Some(input), Some(output), Some(reasoning), Some(cache_read), Some(cache_write)) = (
        usage.input_fresh().value(),
        usage.output().value(),
        usage.reasoning().value(),
        usage.cache_read().value(),
        usage.cache_write().value(),
    ) else {
        return Ok(None);
    };
    Ok(Some(TokenUsageInfoDto::new(
        Required::new(bounded_usage_value(input, "input")?),
        Required::new(bounded_usage_value(output, "output")?),
        Required::new(bounded_usage_value(reasoning, "reasoning")?),
        Required::new(TokenUsageInfoDtoCache::new(
            Required::new(bounded_usage_value(cache_read, "cache-read-input")?),
            Required::new(bounded_usage_value(cache_write, "cache-write-input")?),
        )),
    )))
}

/// The compatibility live event couples tokens and cost as two required
/// coordinates. Keep them separate until both are observed: complete token
/// usage must not manufacture a zero-dollar cost, while a separately known
/// zero remains a real cost fact.
pub(super) fn costed_usage_dto(
    usage: TokenUsage,
    cost: Option<MoneyUsd>,
) -> Result<Option<(MoneyUSDDto, TokenUsageInfoDto)>, ExecutionRefusal> {
    let Some(cost) = cost else {
        return Ok(None);
    };
    let Some(usage) = usage_dto(usage)? else {
        return Ok(None);
    };
    let cost = FiniteF64::try_new(cost.amount())
        .map_err(|error| ExecutionRefusal::Run(error.to_owned()))?;
    Ok(Some((MoneyUSDDto::new(cost), usage)))
}

#[cfg(test)]
fn required_usage(value: Option<u64>, context: &str, field: &str) -> Result<u64, String> {
    value.ok_or_else(|| format!("{context} has absent provider usage field {field}"))
}

fn bounded_usage(value: u64, field: &str) -> Result<u64, String> {
    TokenCount::new(value)
        .map(TokenCount::value)
        .map_err(|error| format!("provider usage {field} is not carriable: {error}"))
}

fn checked_usage_add(current: u64, next: u64, field: &str) -> Result<u64, String> {
    let current = TokenCount::new(current)
        .map_err(|error| format!("provider usage {field} aggregate is invalid: {error}"))?;
    let next = TokenCount::new(next)
        .map_err(|error| format!("provider usage {field} aggregate is invalid: {error}"))?;
    current
        .checked_add(next)
        .map(TokenCount::value)
        .map_err(|error| format!("provider usage {field} aggregate is invalid: {error}"))
}

pub(super) fn usage_float(value: u64, field: &str) -> Result<f64, String> {
    bounded_usage(value, field)?;
    // Every integer through 2^53 is represented exactly by IEEE-754 f64.
    Ok(value as f64)
}

fn bounded_usage_value(value: u64, field: &str) -> Result<FiniteF64, ExecutionRefusal> {
    bounded_usage(value, field).map_err(ExecutionRefusal::Run)?;
    let value = i64::try_from(value).map_err(|_| {
        ExecutionRefusal::Run(format!(
            "provider usage {field} value cannot enter the public integer projection"
        ))
    })?;
    FiniteF64::try_new(value as f64).map_err(|error| ExecutionRefusal::Run(error.to_owned()))
}

#[cfg(test)]
mod tests {
    use grapt_provider::TokenUsage;
    use grapt_types::usage::{UsageDerivation, UsageProtocolFamily};
    use grapt_types::{AttemptUsage, RecordedAttemptUsage, UsageField, UsageObservationPhase};

    use super::*;

    fn reported(value: u64) -> UsageField {
        match TokenCount::new(value) {
            Ok(count) => UsageField::reported(count),
            Err(error) => panic!("fixture token count is bounded: {error}"),
        }
    }

    fn complete(value: u64) -> TokenUsage {
        TokenUsage::new(
            reported(value),
            reported(value),
            reported(value),
            reported(value),
            reported(value),
            reported(value),
            reported(value),
        )
    }

    fn observation(
        input: UsageField,
        output: UsageField,
        reasoning: UsageField,
        cache_read: UsageField,
        cache_write: UsageField,
    ) -> RecordedAttemptUsage {
        RecordedAttemptUsage::V2(AttemptUsage::observed(
            UsageField::ABSENT,
            input,
            output,
            reasoning,
            cache_read,
            cache_write,
            UsageField::ABSENT,
            UsageObservationPhase::Terminal,
        ))
    }

    #[test]
    fn observed_projection_retains_partial_fields_and_refuses_unsafe_counts() -> Result<(), String>
    {
        let partial = [observation(
            reported(19),
            reported(3),
            UsageField::ABSENT,
            reported(0),
            UsageField::ABSENT,
        )];
        let projected = serde_json::to_value(observed_usage_dto(&partial)?)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            projected,
            serde_json::json!({"inputFresh":19,"output":3,"cacheRead":0})
        );
        let unsafe_count = [observation(
            reported(PROVIDER_USAGE_TOKEN_CEILING),
            reported(0),
            reported(0),
            reported(0),
            reported(0),
        )];
        assert!(observed_usage_dto(&unsafe_count).is_err());
        Ok(())
    }

    #[test]
    fn partial_observation_contributes_each_reported_field_without_becoming_complete(
    ) -> Result<(), String> {
        let observations = [observation(
            reported(19),
            reported(7),
            UsageField::ABSENT,
            reported(128),
            UsageField::ABSENT,
        )];
        let reconciled = reconciled_usage(&observations)?;
        assert!(reconciled.complete_if_observed().is_none());
        let total = CompleteUsage::ZERO.checked_add(reconciled.recorded_contribution())?;
        assert_eq!(
            total,
            CompleteUsage {
                input: 19,
                output: 7,
                reasoning: 0,
                cache_read: 128,
                cache_write: 0
            }
        );
        assert!(
            reconciled_usage(&observations)?
                .complete_if_observed()
                .is_none(),
            "aggregation rewrote missing canonical fields"
        );
        Ok(())
    }

    #[test]
    fn live_usage_projection_withholds_absence_preserves_reported_values_and_refuses_ceiling() {
        let absent = TokenUsage::new(
            reported(0),
            UsageField::ABSENT,
            reported(0),
            reported(0),
            reported(0),
            reported(0),
            reported(0),
        );
        assert!(
            usage_dto(absent)
                .unwrap_or_else(|error| panic!("absence projection failed: {error}"))
                .is_none(),
            "absent provider usage must be withheld, never emitted as observed zero"
        );

        let complete_reported = TokenUsage::new(
            reported(11),
            reported(8),
            reported(4),
            reported(3),
            reported(2),
            reported(1),
            reported(15),
        );
        let dto = usage_dto(complete_reported)
            .unwrap_or_else(|error| panic!("reported usage projection failed: {error}"))
            .unwrap_or_else(|| panic!("complete reported usage was withheld"));
        let encoded = serde_json::to_value(dto)
            .unwrap_or_else(|error| panic!("usage DTO serialization failed: {error}"));
        assert_eq!(encoded["input"], 8.0, "public input is fresh input");
        assert_eq!(encoded["output"], 4.0);
        assert_eq!(encoded["reasoning"], 3.0);
        assert_eq!(encoded["cache"]["read"], 2.0);
        assert_eq!(encoded["cache"]["write"], 1.0);

        assert!(usage_dto(complete(PROVIDER_USAGE_TOKEN_CEILING)).is_ok());
        let over = match PROVIDER_USAGE_TOKEN_CEILING.checked_add(1) {
            Some(over) => over,
            None => panic!("the provider usage ceiling leaves room for a counterexample"),
        };
        let refusal = TokenCount::new(over)
            .err()
            .unwrap_or_else(|| panic!("over-bound provider usage was accepted"));
        assert_eq!(
            refusal,
            grapt_types::TokenCountError::AboveExactBound { value: over }
        );
        assert!(
            bounded_usage_value(over, "input").is_err(),
            "the public DTO conversion bypassed the bounded carrier"
        );
    }

    #[test]
    fn reconciliation_preserves_absence_zero_and_reported_reasoning_cache() {
        let absent = reconciled_usage(&[observation(
            UsageField::ABSENT,
            reported(0),
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
        )])
        .unwrap_or_else(|error| panic!("absent usage reconciliation failed: {error}"));
        assert_eq!(absent.input, None);
        assert_eq!(absent.output, Some(0));
        assert_eq!(absent.reasoning, None);
        assert_eq!(absent.complete_if_observed(), None);

        let reported = reconciled_usage(&[observation(
            reported(11),
            reported(4),
            reported(3),
            reported(2),
            reported(1),
        )])
        .unwrap_or_else(|error| panic!("reported usage reconciliation failed: {error}"));
        assert_eq!(reported.reasoning, Some(3));
        assert_eq!(reported.cache_read, Some(2));
        assert_eq!(reported.cache_write, Some(1));
        assert_eq!(
            reported.complete_if_observed(),
            Some(CompleteUsage {
                input: 11,
                output: 4,
                reasoning: 3,
                cache_read: 2,
                cache_write: 1,
            })
        );
        assert_eq!(
            reported
                .complete("reported witness")
                .unwrap_or_else(|error| panic!("complete usage was refused: {error}")),
            CompleteUsage {
                input: 11,
                output: 4,
                reasoning: 3,
                cache_read: 2,
                cache_write: 1,
            }
        );
    }

    fn derived(value: u64, rule: UsageDerivation) -> UsageField {
        match TokenCount::new(value) {
            Ok(count) => UsageField::derived(count, rule),
            Err(error) => panic!("fixture token count is bounded: {error}"),
        }
    }

    /// Build one observation from a family-shaped row: stamped or
    /// unstamped, with the ruling's six coordinates.
    #[allow(clippy::too_many_arguments)]
    fn shape(
        family: Option<UsageProtocolFamily>,
        input_total: UsageField,
        input_fresh: UsageField,
        output: UsageField,
        reasoning: UsageField,
        cache_read: UsageField,
        cache_write: UsageField,
    ) -> RecordedAttemptUsage {
        match family {
            Some(family) => RecordedAttemptUsage::V2(AttemptUsage::observed_stamped(
                input_total,
                input_fresh,
                output,
                reasoning,
                cache_read,
                cache_write,
                UsageField::ABSENT,
                UsageObservationPhase::Terminal,
                family,
            )),
            None => RecordedAttemptUsage::V2(AttemptUsage::observed(
                input_total,
                input_fresh,
                output,
                reasoning,
                cache_read,
                cache_write,
                UsageField::ABSENT,
                UsageObservationPhase::Terminal,
            )),
        }
    }

    fn total(
        input: u64,
        output: u64,
        reasoning: u64,
        cache_read: u64,
        cache_write: u64,
    ) -> CompleteUsage {
        CompleteUsage {
            input,
            output,
            reasoning,
            cache_read,
            cache_write,
        }
    }

    /// The family-shaped rows the ruling's accounting has to resolve: red
    /// (a)/(b) the Cohere no-cache shape with and without the stamp, red (f)
    /// the compatible shape with omitted cache details, the partition pair
    /// (stamped row vs the recorded derivation inference), and the
    /// remainder-equality shape. An absent reasoning stays unknown except
    /// where a recorded guarantee proves its zero.
    #[test]
    fn stamped_and_unstamped_family_shapes_resolve_the_ruling_coordinates() -> Result<(), String> {
        let no_cache = |family| {
            shape(
                family,
                reported(11),
                derived(11, UsageDerivation::FreshFromInclusiveInput),
                reported(4),
                UsageField::ABSENT,
                UsageField::ABSENT,
                UsageField::ABSENT,
            )
        };
        let compatible = |family, input_fresh, reasoning, cache_read, cache_write| {
            shape(
                family,
                reported(11),
                input_fresh,
                reported(4),
                reasoning,
                cache_read,
                cache_write,
            )
        };
        let partition = |family| {
            shape(
                family,
                derived(11, UsageDerivation::InclusiveInputFromPartition),
                reported(8),
                reported(4),
                UsageField::ABSENT,
                reported(2),
                reported(1),
            )
        };
        // The eight ruling rows, named for the case it pins: red (a)/(b)
        // Cohere no-cache stamped and unstamped, red (f) the compatible
        // omissions, the partition pair, and the remainder shape.
        let red_a = no_cache(Some(UsageProtocolFamily::InclusiveInputNoCache));
        let red_b = no_cache(None);
        let red_f_none = compatible(
            Some(UsageProtocolFamily::InclusiveInput),
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
        );
        let red_f_read = compatible(
            Some(UsageProtocolFamily::InclusiveInput),
            derived(9, UsageDerivation::FreshFromInclusiveInput),
            UsageField::ABSENT,
            reported(2),
            UsageField::ABSENT,
        );
        let red_f_full = compatible(
            Some(UsageProtocolFamily::InclusiveInput),
            derived(8, UsageDerivation::FreshFromInclusiveInput),
            reported(3),
            reported(2),
            reported(1),
        );
        let partition_stamped = partition(Some(UsageProtocolFamily::FreshInputPartition));
        let partition_unstamped = partition(None);
        let remainder = shape(
            Some(UsageProtocolFamily::InclusiveInput),
            reported(11),
            derived(9, UsageDerivation::FreshFromInclusiveInput),
            derived(4, UsageDerivation::OutputFromTotalLessInput),
            reported(1),
            reported(2),
            UsageField::ABSENT,
        );
        let cases = [
            // Red (a): the stamped Cohere no-cache shape: protocol zeros, exact.
            (red_a, Some(total(11, 4, 0, 0, 0))),
            // Red (b): unstamped: the equations cannot prove the omitted zeros.
            (red_b, None),
            // Red (f): no cache details at all: fresh input is undervivable.
            (red_f_none, None),
            // Red (f): read observed, write omitted: no remainder equation.
            (red_f_read, None),
            // Red (f): every coordinate observed: the total is exact.
            (red_f_full, Some(total(8, 4, 3, 2, 1))),
            // Stamped partition: the recorded inference with the same result.
            (partition_stamped, Some(total(8, 4, 0, 2, 1))),
            // Unstamped partition: the pre-field inference path, unchanged.
            (partition_unstamped, Some(total(8, 4, 0, 2, 1))),
            // Remainder shape: the one provable zero, nothing else invented.
            (remainder, Some(total(9, 4, 1, 2, 0))),
        ];
        for (observation, expected) in cases {
            assert_eq!(aggregate_usage(&[observation])?, expected);
        }
        Ok(())
    }

    /// Red (g): two different families in one attempt make the total
    /// unavailable, never an error.
    #[test]
    fn two_stamps_in_one_attempt_make_the_total_unavailable_not_an_error() -> Result<(), String> {
        let first = shape(
            Some(UsageProtocolFamily::FreshInputPartition),
            UsageField::ABSENT,
            reported(8),
            reported(4),
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
        );
        let second = shape(
            Some(UsageProtocolFamily::InclusiveInput),
            UsageField::ABSENT,
            reported(8),
            reported(4),
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
        );
        assert_eq!(aggregate_usage(&[first, second])?, None);
        Ok(())
    }

    #[test]
    fn bounded_carrier_and_aggregation_refuse_crossing_the_exact_numeric_ceiling() {
        let over = match PROVIDER_USAGE_TOKEN_CEILING.checked_add(1) {
            Some(over) => over,
            None => panic!("the provider usage ceiling leaves room for a counterexample"),
        };
        let refusal = TokenCount::new(over)
            .err()
            .unwrap_or_else(|| panic!("over-bound usage observation was accepted"));
        assert_eq!(
            refusal,
            grapt_types::TokenCountError::AboveExactBound { value: over }
        );
        assert!(
            usage_float(over, "input").is_err(),
            "the public float projection accepted an over-bound count"
        );

        let at_bound = CompleteUsage {
            input: PROVIDER_USAGE_TOKEN_CEILING,
            output: 0,
            reasoning: 0,
            cache_read: 0,
            cache_write: 0,
        };
        let one = CompleteUsage {
            input: 1,
            output: 0,
            reasoning: 0,
            cache_read: 0,
            cache_write: 0,
        };
        let refusal = at_bound
            .checked_add(one)
            .err()
            .unwrap_or_else(|| panic!("usage aggregation crossed its ceiling"));
        assert!(
            refusal.contains("aggregate")
                && refusal.contains("a sum of token counts exceeded")
                && refusal.contains("refused rather than saturated or wrapped"),
            "usage aggregation failed for the wrong reason: {refusal}"
        );
    }

    #[test]
    fn cost_truth_is_not_inferred_from_complete_tokens() {
        let usage = complete(1);
        assert!(
            costed_usage_dto(usage.clone(), None)
                .unwrap_or_else(|error| panic!("missing-cost projection failed: {error}"))
                .is_none(),
            "complete tokens manufactured a zero-dollar cost"
        );
        assert!(
            costed_usage_dto(usage, Some(MoneyUsd::ZERO))
                .unwrap_or_else(|error| panic!("known-zero projection failed: {error}"))
                .is_some(),
            "a separately known zero cost was mistaken for absence"
        );
    }
}
