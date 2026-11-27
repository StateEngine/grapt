//! Model-agnostic usage truth for one physical attempt.
//!
//! The foundational `usage` row is the law this module makes structural:
//! absent is not zero, reported is not estimated, and later observations
//! reconcile by field and phase without double charging. PC2 tightens the
//! original shape in two ways:
//!
//! * every observed leaf is a bounded [`TokenCount`] with its own provenance;
//! * fresh input and inclusive input are different coordinates.
//!
//! The second point is load-bearing. Responses-compatible providers report an
//! inclusive input total while Anthropic reports fresh input beside cache
//! reads/writes. A single `input_tokens` counter cannot carry both meanings.

use crate::authority::PolicyVersion;
use crate::usage_compat::TokenCount;

/// The arithmetic rule that produced a leaf the provider did not state
/// directly. The vocabulary is provider-neutral: routes choose a rule, but no
/// provider identity enters durable usage truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageDerivation {
    /// `inclusive_input = prompt + provider_tool_use_prompt`.
    InputFromPromptAndToolUse,
    /// `output = total - inclusive_input`.
    OutputFromTotalLessInput,
    /// `fresh = inclusive - cache_read - cache_write`.
    FreshFromInclusiveInput,
    /// `inclusive = fresh + cache_read + cache_write`.
    InclusiveInputFromPartition,
}

/// The provider protocol's accounting model, stamped by the route that
/// observed the usage. The vocabulary is provider-neutral: routes stamp a
/// family, but no provider identity enters durable usage truth.
///
/// The family records what the protocol guarantees about a coordinate the
/// provider omitted, so a reader can tell a protocol zero from an unknown:
///
/// * [`UsageProtocolFamily::FreshInputPartition`]: the input counter is
///   FRESH input reported beside the cache coordinates, and the protocols in
///   this family leave a cache counter out when the cache did not contribute.
/// * [`UsageProtocolFamily::InclusiveInputNoCache`]: the input counter is
///   inclusive, and the protocol has no cache-write coordinate and reports
///   the read coordinate only when the cache contributed.
/// * [`UsageProtocolFamily::InclusiveInput`]: the input counter is inclusive
///   and the cache coordinates are optional: an omitted counter does not
///   prove a zero cache and stays unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageProtocolFamily {
    /// Fresh-input partition protocol (the input counter is fresh input
    /// beside the cache coordinates).
    FreshInputPartition,
    /// Inclusive-input protocol with no cache-write coordinate and a read
    /// coordinate reported only when the cache contributed.
    InclusiveInputNoCache,
    /// Inclusive-input protocol whose optional cache coordinates may be
    /// omitted without proving a zero cache.
    InclusiveInput,
}

/// Where one observed usage leaf came from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UsageProvenance {
    /// The provider stated this exact leaf.
    ProviderReported,
    /// The adapter derived this leaf through one named exact equation.
    ProviderDerived { rule: UsageDerivation },
    /// The harness estimated the leaf under a versioned policy.
    HarnessEstimated { policy: PolicyVersion },
}

/// One bounded observed leaf and its provenance.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UsageMeasurement {
    count: TokenCount,
    provenance: UsageProvenance,
}

impl UsageMeasurement {
    pub fn reported(count: TokenCount) -> Self {
        Self {
            count,
            provenance: UsageProvenance::ProviderReported,
        }
    }

    pub fn derived(count: TokenCount, rule: UsageDerivation) -> Self {
        Self {
            count,
            provenance: UsageProvenance::ProviderDerived { rule },
        }
    }

    pub fn estimated(count: TokenCount, policy: PolicyVersion) -> Self {
        Self {
            count,
            provenance: UsageProvenance::HarnessEstimated { policy },
        }
    }

    pub fn count(&self) -> TokenCount {
        self.count
    }

    pub fn provenance(&self) -> &UsageProvenance {
        &self.provenance
    }
}

/// One optional usage leaf. Absence has no provenance because nothing was
/// observed; observed zero is a real measurement carrying `TokenCount::ZERO`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct UsageField(Option<UsageMeasurement>);

impl UsageField {
    pub const ABSENT: UsageField = UsageField(None);

    pub fn reported(count: TokenCount) -> Self {
        Self(Some(UsageMeasurement::reported(count)))
    }

    pub fn derived(count: TokenCount, rule: UsageDerivation) -> Self {
        Self(Some(UsageMeasurement::derived(count, rule)))
    }

    pub fn estimated(count: TokenCount, policy: PolicyVersion) -> Self {
        Self(Some(UsageMeasurement::estimated(count, policy)))
    }

    pub fn measurement(&self) -> Option<&UsageMeasurement> {
        self.0.as_ref()
    }

    pub fn count(&self) -> Option<TokenCount> {
        self.0.as_ref().map(UsageMeasurement::count)
    }

    /// Compatibility accessor for callers that need an unsigned value.
    pub fn value(&self) -> Option<u64> {
        self.count().map(TokenCount::value)
    }

    pub fn provenance(&self) -> Option<&UsageProvenance> {
        self.0.as_ref().map(UsageMeasurement::provenance)
    }

    pub fn is_absent(&self) -> bool {
        self.0.is_none()
    }
}

/// When an observation reached the attempt owner. A reconciled field retains
/// the phase of the observation that won, so later readers never have to infer
/// it from vector position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum UsageObservationPhase {
    DuringStream,
    Terminal,
    Reconciled,
}

/// One newly authored version-two usage observation for a physical attempt.
///
/// `input_tokens` means FRESH input. `input_total_tokens` means inclusive
/// input. The legacy v1 ambiguous input coordinate is deliberately absent from
/// this type, so no v2 constructor or struct literal can name it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AttemptUsage {
    pub input_total_tokens: UsageField,
    pub input_tokens: UsageField,
    pub output_tokens: UsageField,
    pub reasoning_tokens: UsageField,
    pub cache_read_input_tokens: UsageField,
    pub cache_write_input_tokens: UsageField,
    pub total_tokens: UsageField,
    pub phase: UsageObservationPhase,
    /// The protocol family the route stamped. Absent for records written
    /// before the field and for records decoded without a protocol key.
    pub protocol: Option<UsageProtocolFamily>,
}

impl AttemptUsage {
    /// Construct a newly authored v2 observation stamped with the route's
    /// protocol family. New durable writes always stamp; the family is what
    /// makes a later total exact instead of re-inferred from equations.
    #[allow(clippy::too_many_arguments)]
    pub fn observed_stamped(
        input_total_tokens: UsageField,
        input_tokens: UsageField,
        output_tokens: UsageField,
        reasoning_tokens: UsageField,
        cache_read_input_tokens: UsageField,
        cache_write_input_tokens: UsageField,
        total_tokens: UsageField,
        phase: UsageObservationPhase,
        protocol: UsageProtocolFamily,
    ) -> Self {
        Self {
            input_total_tokens,
            input_tokens,
            output_tokens,
            reasoning_tokens,
            cache_read_input_tokens,
            cache_write_input_tokens,
            total_tokens,
            phase,
            protocol: Some(protocol),
        }
    }

    /// Construct an unstamped v2 observation. Production writers never use
    /// this: it decodes records that predate the protocol field, and tests
    /// model legacy records with it.
    #[allow(clippy::too_many_arguments)]
    pub fn observed(
        input_total_tokens: UsageField,
        input_tokens: UsageField,
        output_tokens: UsageField,
        reasoning_tokens: UsageField,
        cache_read_input_tokens: UsageField,
        cache_write_input_tokens: UsageField,
        total_tokens: UsageField,
        phase: UsageObservationPhase,
    ) -> Self {
        Self {
            input_total_tokens,
            input_tokens,
            output_tokens,
            reasoning_tokens,
            cache_read_input_tokens,
            cache_write_input_tokens,
            total_tokens,
            phase,
            protocol: None,
        }
    }

    pub fn input_total_tokens(&self) -> &UsageField {
        &self.input_total_tokens
    }

    pub fn input_tokens(&self) -> &UsageField {
        &self.input_tokens
    }

    pub fn output_tokens(&self) -> &UsageField {
        &self.output_tokens
    }

    pub fn reasoning_tokens(&self) -> &UsageField {
        &self.reasoning_tokens
    }

    pub fn cache_read_input_tokens(&self) -> &UsageField {
        &self.cache_read_input_tokens
    }

    pub fn cache_write_input_tokens(&self) -> &UsageField {
        &self.cache_write_input_tokens
    }

    pub fn total_tokens(&self) -> &UsageField {
        &self.total_tokens
    }

    pub fn phase(&self) -> UsageObservationPhase {
        self.phase
    }

    pub fn protocol(&self) -> Option<UsageProtocolFamily> {
        self.protocol
    }
}

/// Decoder/migration-only truth for the shipped v1 payload.
///
/// This is a different type from [`AttemptUsage`]. Its single input counter is
/// explicitly ambiguous and cannot be passed to a v2 event constructor or
/// encoder.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LegacyAttemptUsage {
    input_tokens_ambiguous: UsageField,
    output_tokens: UsageField,
    reasoning_tokens: UsageField,
    cache_read_input_tokens: UsageField,
    cache_write_input_tokens: UsageField,
    phase: UsageObservationPhase,
}

impl LegacyAttemptUsage {
    /// Construct only while decoding or migrating a shipped v1 record whose
    /// route-dependent input meaning was not persisted.
    #[allow(clippy::too_many_arguments)]
    pub fn from_v1_migration(
        input_tokens_ambiguous: UsageField,
        output_tokens: UsageField,
        reasoning_tokens: UsageField,
        cache_read_input_tokens: UsageField,
        cache_write_input_tokens: UsageField,
        phase: UsageObservationPhase,
    ) -> Self {
        Self {
            input_tokens_ambiguous,
            output_tokens,
            reasoning_tokens,
            cache_read_input_tokens,
            cache_write_input_tokens,
            phase,
        }
    }

    pub fn input_tokens_ambiguous(&self) -> &UsageField {
        &self.input_tokens_ambiguous
    }

    pub fn output_tokens(&self) -> &UsageField {
        &self.output_tokens
    }

    pub fn reasoning_tokens(&self) -> &UsageField {
        &self.reasoning_tokens
    }

    pub fn cache_read_input_tokens(&self) -> &UsageField {
        &self.cache_read_input_tokens
    }

    pub fn cache_write_input_tokens(&self) -> &UsageField {
        &self.cache_write_input_tokens
    }

    pub fn phase(&self) -> UsageObservationPhase {
        self.phase
    }
}

/// Replay input for a history that may contain both v2 and shipped v1 rows.
/// The enum is never accepted by a v2 encoder; it exists only at the fold edge.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RecordedAttemptUsage {
    V2(AttemptUsage),
    LegacyV1(LegacyAttemptUsage),
}

impl RecordedAttemptUsage {
    pub fn phase(&self) -> UsageObservationPhase {
        match self {
            Self::V2(usage) => usage.phase(),
            Self::LegacyV1(usage) => usage.phase(),
        }
    }

    pub fn input_total_tokens(&self) -> Option<&UsageField> {
        match self {
            Self::V2(usage) => Some(usage.input_total_tokens()),
            Self::LegacyV1(_) => None,
        }
    }

    pub fn input_tokens(&self) -> Option<&UsageField> {
        match self {
            Self::V2(usage) => Some(usage.input_tokens()),
            Self::LegacyV1(_) => None,
        }
    }

    pub fn input_tokens_ambiguous(&self) -> Option<&UsageField> {
        match self {
            Self::V2(_) => None,
            Self::LegacyV1(usage) => Some(usage.input_tokens_ambiguous()),
        }
    }

    pub fn output_tokens(&self) -> &UsageField {
        match self {
            Self::V2(usage) => usage.output_tokens(),
            Self::LegacyV1(usage) => usage.output_tokens(),
        }
    }

    pub fn reasoning_tokens(&self) -> &UsageField {
        match self {
            Self::V2(usage) => usage.reasoning_tokens(),
            Self::LegacyV1(usage) => usage.reasoning_tokens(),
        }
    }

    pub fn cache_read_input_tokens(&self) -> &UsageField {
        match self {
            Self::V2(usage) => usage.cache_read_input_tokens(),
            Self::LegacyV1(usage) => usage.cache_read_input_tokens(),
        }
    }

    pub fn cache_write_input_tokens(&self) -> &UsageField {
        match self {
            Self::V2(usage) => usage.cache_write_input_tokens(),
            Self::LegacyV1(usage) => usage.cache_write_input_tokens(),
        }
    }

    pub fn total_tokens(&self) -> Option<&UsageField> {
        match self {
            Self::V2(usage) => Some(usage.total_tokens()),
            Self::LegacyV1(_) => None,
        }
    }
}

/// One reconciled leaf, retaining the winning observation's phase and
/// provenance.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReconciledUsageField {
    measurement: UsageMeasurement,
    phase: UsageObservationPhase,
}

impl ReconciledUsageField {
    pub fn count(&self) -> TokenCount {
        self.measurement.count()
    }

    pub fn provenance(&self) -> &UsageProvenance {
        self.measurement.provenance()
    }

    pub fn phase(&self) -> UsageObservationPhase {
        self.phase
    }
}

/// The non-double-charging fold of one attempt's observations.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReconciledUsage {
    input_total: Option<ReconciledUsageField>,
    input_fresh: Option<ReconciledUsageField>,
    legacy_input: Option<ReconciledUsageField>,
    output: Option<ReconciledUsageField>,
    reasoning: Option<ReconciledUsageField>,
    cache_read: Option<ReconciledUsageField>,
    cache_write: Option<ReconciledUsageField>,
    total: Option<ReconciledUsageField>,
}

impl ReconciledUsage {
    pub fn input_total(&self) -> Option<&ReconciledUsageField> {
        self.input_total.as_ref()
    }

    pub fn input_fresh(&self) -> Option<&ReconciledUsageField> {
        self.input_fresh.as_ref()
    }

    pub fn legacy_input(&self) -> Option<&ReconciledUsageField> {
        self.legacy_input.as_ref()
    }

    pub fn output(&self) -> Option<&ReconciledUsageField> {
        self.output.as_ref()
    }

    pub fn reasoning(&self) -> Option<&ReconciledUsageField> {
        self.reasoning.as_ref()
    }

    pub fn cache_read(&self) -> Option<&ReconciledUsageField> {
        self.cache_read.as_ref()
    }

    pub fn cache_write(&self) -> Option<&ReconciledUsageField> {
        self.cache_write.as_ref()
    }

    pub fn total(&self) -> Option<&ReconciledUsageField> {
        self.total.as_ref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageFieldKind {
    InputTotal,
    InputFresh,
    LegacyInputAmbiguous,
    Output,
    Reasoning,
    CacheRead,
    CacheWrite,
    Total,
}

impl core::fmt::Display for UsageFieldKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::InputTotal => "input-total",
            Self::InputFresh => "input-fresh",
            Self::LegacyInputAmbiguous => "legacy-input-ambiguous",
            Self::Output => "output",
            Self::Reasoning => "reasoning",
            Self::CacheRead => "cache-read-input",
            Self::CacheWrite => "cache-write-input",
            Self::Total => "total",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageReconciliationError {
    pub field: UsageFieldKind,
    pub phase: UsageObservationPhase,
    pub earlier: TokenCount,
    pub later: TokenCount,
}

impl core::fmt::Display for UsageReconciliationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "usage field {} has conflicting {:?} observations {} and {}",
            self.field,
            self.phase,
            self.earlier.value(),
            self.later.value()
        )
    }
}

impl std::error::Error for UsageReconciliationError {}

/// Reconcile later phases by leaf. Same-phase conflicting values are durable
/// corruption rather than last-write-wins. Same-value observations retain the
/// strongest provenance without adding the count twice.
pub fn reconcile_usage(
    observations: &[AttemptUsage],
) -> Result<ReconciledUsage, UsageReconciliationError> {
    let mut reconciled = ReconciledUsage::default();
    for observation in observations {
        merge_v2(&mut reconciled, observation)?;
    }
    Ok(reconciled)
}

/// Reconcile durable history while preserving the v1 input counter in the
/// explicitly ambiguous migration coordinate. A legacy count is never added
/// to fresh or inclusive input and therefore cannot be double charged.
pub fn reconcile_recorded_usage(
    observations: &[RecordedAttemptUsage],
) -> Result<ReconciledUsage, UsageReconciliationError> {
    let mut reconciled = ReconciledUsage::default();
    for observation in observations {
        match observation {
            RecordedAttemptUsage::V2(observation) => merge_v2(&mut reconciled, observation)?,
            RecordedAttemptUsage::LegacyV1(observation) => {
                merge(
                    &mut reconciled.legacy_input,
                    &observation.input_tokens_ambiguous,
                    observation.phase,
                    UsageFieldKind::LegacyInputAmbiguous,
                )?;
                merge(
                    &mut reconciled.output,
                    &observation.output_tokens,
                    observation.phase,
                    UsageFieldKind::Output,
                )?;
                merge(
                    &mut reconciled.reasoning,
                    &observation.reasoning_tokens,
                    observation.phase,
                    UsageFieldKind::Reasoning,
                )?;
                merge(
                    &mut reconciled.cache_read,
                    &observation.cache_read_input_tokens,
                    observation.phase,
                    UsageFieldKind::CacheRead,
                )?;
                merge(
                    &mut reconciled.cache_write,
                    &observation.cache_write_input_tokens,
                    observation.phase,
                    UsageFieldKind::CacheWrite,
                )?;
            }
        }
    }
    Ok(reconciled)
}

fn merge_v2(
    reconciled: &mut ReconciledUsage,
    observation: &AttemptUsage,
) -> Result<(), UsageReconciliationError> {
    merge(
        &mut reconciled.input_total,
        &observation.input_total_tokens,
        observation.phase,
        UsageFieldKind::InputTotal,
    )?;
    merge(
        &mut reconciled.input_fresh,
        &observation.input_tokens,
        observation.phase,
        UsageFieldKind::InputFresh,
    )?;
    merge(
        &mut reconciled.output,
        &observation.output_tokens,
        observation.phase,
        UsageFieldKind::Output,
    )?;
    merge(
        &mut reconciled.reasoning,
        &observation.reasoning_tokens,
        observation.phase,
        UsageFieldKind::Reasoning,
    )?;
    merge(
        &mut reconciled.cache_read,
        &observation.cache_read_input_tokens,
        observation.phase,
        UsageFieldKind::CacheRead,
    )?;
    merge(
        &mut reconciled.cache_write,
        &observation.cache_write_input_tokens,
        observation.phase,
        UsageFieldKind::CacheWrite,
    )?;
    merge(
        &mut reconciled.total,
        &observation.total_tokens,
        observation.phase,
        UsageFieldKind::Total,
    )?;
    Ok(())
}

fn merge(
    current: &mut Option<ReconciledUsageField>,
    incoming: &UsageField,
    phase: UsageObservationPhase,
    field: UsageFieldKind,
) -> Result<(), UsageReconciliationError> {
    let Some(measurement) = incoming.measurement() else {
        return Ok(());
    };
    let next = ReconciledUsageField {
        measurement: measurement.clone(),
        phase,
    };
    let Some(held) = current else {
        *current = Some(next);
        return Ok(());
    };
    if phase > held.phase {
        *held = next;
        return Ok(());
    }
    if phase < held.phase {
        return Ok(());
    }
    if held.count() != next.count() {
        return Err(UsageReconciliationError {
            field,
            phase,
            earlier: held.count(),
            later: next.count(),
        });
    }
    if provenance_rank(next.provenance()) > provenance_rank(held.provenance()) {
        *held = next;
    }
    Ok(())
}

fn provenance_rank(provenance: &UsageProvenance) -> u8 {
    match provenance {
        UsageProvenance::HarnessEstimated { .. } => 0,
        UsageProvenance::ProviderDerived { .. } => 1,
        UsageProvenance::ProviderReported => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(value: u64) -> TokenCount {
        TokenCount::new(value).unwrap_or_else(|error| panic!("test count: {error}"))
    }

    #[test]
    fn absence_zero_and_leaf_provenance_are_distinct() {
        let absent = UsageField::ABSENT;
        let zero = UsageField::reported(TokenCount::ZERO);
        let derived = UsageField::derived(count(0), UsageDerivation::FreshFromInclusiveInput);
        assert!(absent.is_absent());
        assert_eq!(absent.value(), None);
        assert_eq!(zero.value(), Some(0));
        assert_eq!(derived.value(), Some(0));
        assert_ne!(zero, derived);
    }

    #[test]
    fn terminal_reconciles_stream_without_double_charging_and_keeps_leaf_origin() {
        let streamed = AttemptUsage::observed(
            UsageField::ABSENT,
            UsageField::reported(count(8)),
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageObservationPhase::DuringStream,
        );
        let terminal = AttemptUsage::observed(
            UsageField::reported(count(11)),
            UsageField::derived(count(8), UsageDerivation::FreshFromInclusiveInput),
            UsageField::reported(count(4)),
            UsageField::reported(count(3)),
            UsageField::reported(count(2)),
            UsageField::reported(count(1)),
            UsageField::reported(count(15)),
            UsageObservationPhase::Terminal,
        );
        let usage = reconcile_usage(&[streamed, terminal])
            .unwrap_or_else(|error| panic!("reconcile: {error}"));
        let fresh = usage
            .input_fresh()
            .unwrap_or_else(|| panic!("fresh input absent"));
        assert_eq!(fresh.count().value(), 8);
        assert_eq!(fresh.phase(), UsageObservationPhase::Terminal);
        assert!(matches!(
            fresh.provenance(),
            UsageProvenance::ProviderDerived {
                rule: UsageDerivation::FreshFromInclusiveInput
            }
        ));
    }

    #[test]
    fn same_phase_conflict_is_corruption_and_legacy_input_stays_separate() {
        let first = LegacyAttemptUsage::from_v1_migration(
            UsageField::reported(count(9)),
            UsageField::reported(count(1)),
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageObservationPhase::Terminal,
        );
        let second = LegacyAttemptUsage::from_v1_migration(
            UsageField::reported(count(10)),
            UsageField::reported(count(1)),
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageObservationPhase::Terminal,
        );
        let error = reconcile_recorded_usage(&[
            RecordedAttemptUsage::LegacyV1(first.clone()),
            RecordedAttemptUsage::LegacyV1(second),
        ])
        .err()
        .unwrap_or_else(|| panic!("conflict accepted"));
        assert_eq!(error.field, UsageFieldKind::LegacyInputAmbiguous);

        let v2 = AttemptUsage::observed(
            UsageField::reported(count(11)),
            UsageField::reported(count(8)),
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageObservationPhase::DuringStream,
        );
        let usage = reconcile_recorded_usage(&[
            RecordedAttemptUsage::LegacyV1(first),
            RecordedAttemptUsage::V2(v2),
        ])
        .unwrap_or_else(|error| panic!("legacy: {error}"));
        assert_eq!(
            usage
                .input_fresh()
                .unwrap_or_else(|| panic!("v2 fresh input absent"))
                .count()
                .value(),
            8
        );
        assert_eq!(
            usage
                .input_total()
                .unwrap_or_else(|| panic!("v2 inclusive input absent"))
                .count()
                .value(),
            11
        );
        assert_eq!(
            usage
                .legacy_input()
                .unwrap_or_else(|| panic!("legacy input absent"))
                .count()
                .value(),
            9
        );
    }
}
