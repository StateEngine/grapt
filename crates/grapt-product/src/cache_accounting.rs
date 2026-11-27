//! Product cache measurement over authoritative prefix and usage facts.
//!
//! Prefix eligibility is a local pre-I/O fact. Cache reads and writes are
//! provider observations. A hit is verified only when both agree; neither is
//! allowed to stand in for the other.

use std::collections::BTreeMap;

use grapt_policy::VerifiedPrefixComparison;
use grapt_types::{MoneyUsd, PolicyVersion, PrefixReceiptError, ReconciledUsage, UsageProvenance};
use serde::Serialize;
use thiserror::Error;

pub const MAX_CACHE_PROFILE_BYTES: usize = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheMeasurementBounds {
    max_attempts: u32,
    max_profiles: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CacheBoundsError {
    #[error("cache measurement attempt bound must be nonzero")]
    ZeroAttempts,
    #[error("cache measurement profile bound must be nonzero")]
    ZeroProfiles,
}

impl CacheMeasurementBounds {
    pub fn new(max_attempts: u32, max_profiles: u16) -> Result<Self, CacheBoundsError> {
        if max_attempts == 0 {
            return Err(CacheBoundsError::ZeroAttempts);
        }
        if max_profiles == 0 {
            return Err(CacheBoundsError::ZeroProfiles);
        }
        Ok(Self {
            max_attempts,
            max_profiles,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheProfile(String);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CacheProfileError {
    #[error("cache profile is empty")]
    Empty,
    #[error("cache profile is {offered} bytes; maximum is {maximum}")]
    TooLong { maximum: usize, offered: usize },
}

impl CacheProfile {
    pub fn new(value: impl Into<String>) -> Result<Self, CacheProfileError> {
        let value = value.into();
        if value.is_empty() {
            return Err(CacheProfileError::Empty);
        }
        if value.len() > MAX_CACHE_PROFILE_BYTES {
            return Err(CacheProfileError::TooLong {
                maximum: MAX_CACHE_PROFILE_BYTES,
                offered: value.len(),
            });
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingKeyTruth {
    Present,
    Absent,
    Unsupported,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PrefixTruth {
    Missing,
    FirstObservation,
    Fieldless,
    Verified(VerifiedPrefixComparison),
    Corrupt(PrefixReceiptError),
}

#[derive(Debug, Clone, PartialEq)]
pub enum CostTruth {
    Billed(MoneyUsd),
    Estimated {
        amount: MoneyUsd,
        policy: PolicyVersion,
    },
    Unavailable,
}

#[derive(Debug)]
pub struct CacheAttempt<'a> {
    pub profile: CacheProfile,
    pub routing: RoutingKeyTruth,
    pub prefix: PrefixTruth,
    pub usage: &'a ReconciledUsage,
    pub cost: CostTruth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CacheReadClass {
    Unavailable,
    Zero,
    Positive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CacheWriteClass {
    Unavailable,
    Zero,
    Positive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CostClass {
    Billed,
    Estimated,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EligibilityClass {
    Reusable,
    FirstObservation,
    Invalidated,
    Fieldless,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheAttemptClassification {
    pub eligibility: EligibilityClass,
    pub cache_read: CacheReadClass,
    pub cache_write: CacheWriteClass,
    pub cost: CostClass,
    pub verified_hit: bool,
    pub unattributed_positive_read: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct CacheProfileScore {
    attempts: u32,
    verified_hits: u32,
    read_tokens: u64,
    fresh_tokens: u64,
    write_tokens: u64,
}

impl CacheProfileScore {
    pub fn attempts(&self) -> u32 {
        self.attempts
    }
    pub fn verified_hits(&self) -> u32 {
        self.verified_hits
    }
    pub fn read_tokens(&self) -> u64 {
        self.read_tokens
    }
    pub fn fresh_tokens(&self) -> u64 {
        self.fresh_tokens
    }
    pub fn write_tokens(&self) -> u64 {
        self.write_tokens
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheScorecard {
    schema: &'static str,
    attempts: u32,
    verified_hits: u32,
    misses: u32,
    unattributed_positive_reads: u32,
    unavailable_reads: u32,
    billed_costs: u32,
    estimated_costs: u32,
    unavailable_costs: u32,
    profiles: BTreeMap<String, CacheProfileScore>,
}

impl CacheScorecard {
    pub fn new() -> Self {
        Self {
            schema: "grapt.cache.scorecard.v1",
            attempts: 0,
            verified_hits: 0,
            misses: 0,
            unattributed_positive_reads: 0,
            unavailable_reads: 0,
            billed_costs: 0,
            estimated_costs: 0,
            unavailable_costs: 0,
            profiles: BTreeMap::new(),
        }
    }
    pub fn attempts(&self) -> u32 {
        self.attempts
    }
    pub fn verified_hits(&self) -> u32 {
        self.verified_hits
    }
    pub fn misses(&self) -> u32 {
        self.misses
    }
    pub fn unattributed_positive_reads(&self) -> u32 {
        self.unattributed_positive_reads
    }
    pub fn unavailable_reads(&self) -> u32 {
        self.unavailable_reads
    }
    pub fn billed_costs(&self) -> u32 {
        self.billed_costs
    }
    pub fn estimated_costs(&self) -> u32 {
        self.estimated_costs
    }
    pub fn unavailable_costs(&self) -> u32 {
        self.unavailable_costs
    }
    pub fn profile(&self, name: &str) -> Option<&CacheProfileScore> {
        self.profiles.get(name)
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn observe(
        &mut self,
        bounds: CacheMeasurementBounds,
        attempt: CacheAttempt<'_>,
    ) -> Result<CacheAttemptClassification, CacheMeasurementError> {
        if let PrefixTruth::Corrupt(reason) = &attempt.prefix {
            return Err(CacheMeasurementError::CorruptPrefix(reason.clone()));
        }
        let next_attempts = self
            .attempts
            .checked_add(1)
            .ok_or(CacheMeasurementError::ArithmeticOverflow { field: "attempts" })?;
        if next_attempts > bounds.max_attempts {
            return Err(CacheMeasurementError::AttemptBound {
                limit: bounds.max_attempts,
                offered: next_attempts,
            });
        }
        if !self.profiles.contains_key(attempt.profile.as_str())
            && self.profiles.len() >= usize::from(bounds.max_profiles)
        {
            return Err(CacheMeasurementError::ProfileBound {
                limit: bounds.max_profiles,
                offered: self.profiles.len().checked_add(1).ok_or(
                    CacheMeasurementError::ArithmeticOverflow {
                        field: "profile-count",
                    },
                )?,
            });
        }

        let eligibility = match &attempt.prefix {
            PrefixTruth::Missing => EligibilityClass::Missing,
            PrefixTruth::FirstObservation => EligibilityClass::FirstObservation,
            PrefixTruth::Fieldless => EligibilityClass::Fieldless,
            PrefixTruth::Verified(comparison) if comparison.is_reusable() => {
                EligibilityClass::Reusable
            }
            PrefixTruth::Verified(_) => EligibilityClass::Invalidated,
            PrefixTruth::Corrupt(_) => return Err(CacheMeasurementError::UnreachableCorruption),
        };
        let (read, read_count, read_reported) = classify_usage(attempt.usage.cache_read());
        let (write, write_count, _) = classify_write(attempt.usage.cache_write());
        let verified_hit = attempt.routing == RoutingKeyTruth::Present
            && eligibility == EligibilityClass::Reusable
            && read == CacheReadClass::Positive
            && read_reported;
        let unattributed_positive_read = read == CacheReadClass::Positive && !verified_hit;
        let cost = match &attempt.cost {
            CostTruth::Billed(_) => CostClass::Billed,
            CostTruth::Estimated { .. } => CostClass::Estimated,
            CostTruth::Unavailable => CostClass::Unavailable,
        };

        // Stage every fallible delta against immutable state. No scorecard
        // field or profile entry is committed until every checked add and
        // bound has succeeded, so every refusal is byte-for-byte atomic.
        let held_profile = self
            .profiles
            .get(attempt.profile.as_str())
            .cloned()
            .unwrap_or_default();
        let next_profile = CacheProfileScore {
            attempts: add_u32(held_profile.attempts, 1, "profile-attempts")?,
            verified_hits: add_u32(
                held_profile.verified_hits,
                u32::from(verified_hit),
                "profile-verified-hits",
            )?,
            read_tokens: add_u64(held_profile.read_tokens, read_count, "profile-read-tokens")?,
            fresh_tokens: add_u64(
                held_profile.fresh_tokens,
                attempt
                    .usage
                    .input_fresh()
                    .map_or(0, |field| field.count().value()),
                "profile-fresh-tokens",
            )?,
            write_tokens: add_u64(
                held_profile.write_tokens,
                write_count,
                "profile-write-tokens",
            )?,
        };
        let next_verified_hits =
            add_u32(self.verified_hits, u32::from(verified_hit), "verified-hits")?;
        let next_misses = add_u32(self.misses, u32::from(!verified_hit), "misses")?;
        let next_unattributed_positive_reads = add_u32(
            self.unattributed_positive_reads,
            u32::from(unattributed_positive_read),
            "unattributed-positive-reads",
        )?;
        let next_unavailable_reads = add_u32(
            self.unavailable_reads,
            u32::from(read == CacheReadClass::Unavailable),
            "unavailable-reads",
        )?;
        let next_billed_costs = add_u32(
            self.billed_costs,
            u32::from(cost == CostClass::Billed),
            "billed-costs",
        )?;
        let next_estimated_costs = add_u32(
            self.estimated_costs,
            u32::from(cost == CostClass::Estimated),
            "estimated-costs",
        )?;
        let next_unavailable_costs = add_u32(
            self.unavailable_costs,
            u32::from(cost == CostClass::Unavailable),
            "unavailable-costs",
        )?;

        self.profiles.insert(attempt.profile.0, next_profile);
        self.attempts = next_attempts;
        self.verified_hits = next_verified_hits;
        self.misses = next_misses;
        self.unattributed_positive_reads = next_unattributed_positive_reads;
        self.unavailable_reads = next_unavailable_reads;
        self.billed_costs = next_billed_costs;
        self.estimated_costs = next_estimated_costs;
        self.unavailable_costs = next_unavailable_costs;

        Ok(CacheAttemptClassification {
            eligibility,
            cache_read: read,
            cache_write: write,
            cost,
            verified_hit,
            unattributed_positive_read,
        })
    }
}

impl Default for CacheScorecard {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CacheMeasurementError {
    #[error("corrupt prefix cache entry: {0}")]
    CorruptPrefix(PrefixReceiptError),
    #[error("cache attempt bound {limit} exceeded by {offered}")]
    AttemptBound { limit: u32, offered: u32 },
    #[error("cache profile bound {limit} exceeded by {offered}")]
    ProfileBound { limit: u16, offered: usize },
    #[error("cache scorecard arithmetic overflowed at {field}")]
    ArithmeticOverflow { field: &'static str },
    #[error("internal corruption branch was reached")]
    UnreachableCorruption,
}

fn classify_usage(
    field: Option<&grapt_types::ReconciledUsageField>,
) -> (CacheReadClass, u64, bool) {
    match field {
        None => (CacheReadClass::Unavailable, 0, false),
        Some(value) if value.count().value() == 0 => (
            CacheReadClass::Zero,
            0,
            matches!(value.provenance(), UsageProvenance::ProviderReported),
        ),
        Some(value) => (
            CacheReadClass::Positive,
            value.count().value(),
            matches!(value.provenance(), UsageProvenance::ProviderReported),
        ),
    }
}

fn classify_write(
    field: Option<&grapt_types::ReconciledUsageField>,
) -> (CacheWriteClass, u64, bool) {
    match field {
        None => (CacheWriteClass::Unavailable, 0, false),
        Some(value) if value.count().value() == 0 => (
            CacheWriteClass::Zero,
            0,
            matches!(value.provenance(), UsageProvenance::ProviderReported),
        ),
        Some(value) => (
            CacheWriteClass::Positive,
            value.count().value(),
            matches!(value.provenance(), UsageProvenance::ProviderReported),
        ),
    }
}

fn add_u32(left: u32, right: u32, field: &'static str) -> Result<u32, CacheMeasurementError> {
    left.checked_add(right)
        .ok_or(CacheMeasurementError::ArithmeticOverflow { field })
}
fn add_u64(left: u64, right: u64, field: &'static str) -> Result<u64, CacheMeasurementError> {
    left.checked_add(right)
        .ok_or(CacheMeasurementError::ArithmeticOverflow { field })
}

#[cfg(test)]
mod tests {
    mod atomic;

    use super::*;
    use grapt_policy::{PrefixIdentityAuthority, PreparedPrefixPart, Secret};
    use grapt_types::{
        reconcile_usage, AdapterVersion, AttemptUsage, ContextEpoch, ContributionId,
        PrefixIdentityBounds, PrefixKeyRealm, PrefixKeyVersion, ProtocolVersion,
        ProviderCacheNamespace, TokenCount, UsageDerivation, UsageField, UsageObservationPhase,
    };
    use serde::Deserialize;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn verified(kind: &str) -> TestResult<VerifiedPrefixComparison> {
        let owner = PrefixIdentityAuthority::new(
            PrefixKeyRealm::new("scorecard")?,
            PrefixKeyVersion::new("v1")?,
            Secret::new(b"scorecard-key".to_vec()),
        );
        let namespace = ProviderCacheNamespace::new("responses")?;
        let adapter = AdapterVersion::new("adapter-1").ok_or("adapter")?;
        let protocol = ProtocolVersion::new("protocol-1").ok_or("protocol")?;
        let policy = PolicyVersion::new("policy-1").ok_or("policy")?;
        let bounds = PrefixIdentityBounds::new(4, 32, 64)?;
        let first = ContributionId::new("first").ok_or("first contribution")?;
        let second = ContributionId::new("second").ok_or("second contribution")?;
        let original = [
            PreparedPrefixPart {
                contribution: &first,
                bytes: b"alpha",
            },
            PreparedPrefixPart {
                contribution: &second,
                bytes: b"beta",
            },
        ];
        let receipt = owner.mint(
            namespace.clone(),
            ContextEpoch::new(1),
            adapter.clone(),
            protocol.clone(),
            policy.clone(),
            bounds,
            &original,
        )?;
        let changed = [
            PreparedPrefixPart {
                contribution: &first,
                bytes: b"changed",
            },
            original[1],
        ];
        let reversed = [original[1], original[0]];
        let (comparer, offered_adapter, offered_protocol, offered_policy, offered_parts) =
            match kind {
                "same" => (
                    &owner,
                    adapter.clone(),
                    protocol.clone(),
                    policy.clone(),
                    &original[..],
                ),
                "contribution-changed" => (
                    &owner,
                    adapter.clone(),
                    protocol.clone(),
                    policy.clone(),
                    &changed[..],
                ),
                "order-changed" => (
                    &owner,
                    adapter.clone(),
                    protocol.clone(),
                    policy.clone(),
                    &reversed[..],
                ),
                "adapter-changed" => (
                    &owner,
                    AdapterVersion::new("adapter-2").ok_or("adapter")?,
                    protocol.clone(),
                    policy.clone(),
                    &original[..],
                ),
                "protocol-changed" => (
                    &owner,
                    adapter.clone(),
                    ProtocolVersion::new("protocol-2").ok_or("protocol")?,
                    policy.clone(),
                    &original[..],
                ),
                "policy-changed" => (
                    &owner,
                    adapter.clone(),
                    protocol.clone(),
                    PolicyVersion::new("policy-2").ok_or("policy")?,
                    &original[..],
                ),
                "key-rotated" => {
                    let replacement = PrefixIdentityAuthority::new(
                        PrefixKeyRealm::new("scorecard")?,
                        PrefixKeyVersion::new("v1")?,
                        Secret::new(b"replacement-key".to_vec()),
                    );
                    return Ok(replacement.compare_committed_prefix(
                        &receipt,
                        &namespace,
                        ContextEpoch::new(1),
                        &adapter,
                        &protocol,
                        &policy,
                        &original,
                    )?);
                }
                other => return Err(format!("unknown comparison fixture {other}").into()),
            };
        Ok(comparer.compare_committed_prefix(
            &receipt,
            &namespace,
            ContextEpoch::new(1),
            &offered_adapter,
            &offered_protocol,
            &offered_policy,
            offered_parts,
        )?)
    }

    fn usage(
        fresh: Option<u64>,
        read: UsageField,
        write: UsageField,
    ) -> TestResult<ReconciledUsage> {
        let fresh = match fresh {
            Some(value) => UsageField::reported(TokenCount::new(value)?),
            None => UsageField::ABSENT,
        };
        Ok(reconcile_usage(&[AttemptUsage::observed(
            UsageField::ABSENT,
            fresh,
            UsageField::ABSENT,
            UsageField::ABSENT,
            read,
            write,
            UsageField::ABSENT,
            UsageObservationPhase::Terminal,
        )])?)
    }

    fn attempt<'a>(
        usage: &'a ReconciledUsage,
        prefix: PrefixTruth,
    ) -> TestResult<CacheAttempt<'a>> {
        attempt_with_cost(usage, prefix, CostTruth::Unavailable)
    }

    fn attempt_with_cost<'a>(
        usage: &'a ReconciledUsage,
        prefix: PrefixTruth,
        cost: CostTruth,
    ) -> TestResult<CacheAttempt<'a>> {
        Ok(CacheAttempt {
            profile: CacheProfile::new("responses")?,
            routing: RoutingKeyTruth::Present,
            prefix,
            usage,
            cost,
        })
    }

    fn assert_atomic_overflow(
        mut scorecard: CacheScorecard,
        usage: &ReconciledUsage,
        prefix: PrefixTruth,
        cost: CostTruth,
        field: &'static str,
    ) -> TestResult {
        let before = serde_json::to_vec(&scorecard)?;
        let result = scorecard.observe(
            CacheMeasurementBounds::new(u32::MAX, u16::MAX)?,
            attempt_with_cost(usage, prefix, cost)?,
        );
        assert_eq!(
            result,
            Err(CacheMeasurementError::ArithmeticOverflow { field }),
            "the boundary attempt refused at the wrong checked coordinate"
        );
        assert_eq!(
            serde_json::to_vec(&scorecard)?,
            before,
            "the scorecard changed before the {field} refusal"
        );
        Ok(())
    }

    #[test]
    fn provider_reported_positive_read_plus_reusable_prefix_is_a_verified_hit() -> TestResult {
        let usage = usage(
            Some(3),
            UsageField::reported(TokenCount::new(7)?),
            UsageField::reported(TokenCount::ZERO),
        )?;
        let mut scorecard = CacheScorecard::new();
        let result = scorecard.observe(
            CacheMeasurementBounds::new(4, 2)?,
            attempt(&usage, PrefixTruth::Verified(verified("same")?))?,
        )?;
        assert!(result.verified_hit);
        assert_eq!(scorecard.verified_hits(), 1);
        assert_eq!(
            scorecard
                .profile("responses")
                .ok_or("profile")?
                .read_tokens(),
            7
        );
        Ok(())
    }

    #[test]
    fn positive_provider_read_without_eligibility_is_not_promoted_to_a_hit() -> TestResult {
        let usage = usage(
            Some(3),
            UsageField::reported(TokenCount::new(7)?),
            UsageField::ABSENT,
        )?;
        let mut scorecard = CacheScorecard::new();
        let result = scorecard.observe(
            CacheMeasurementBounds::new(4, 2)?,
            attempt(&usage, PrefixTruth::Verified(verified("policy-changed")?))?,
        )?;
        assert!(
            !result.verified_hit,
            "an invalidated prefix plus positive provider usage was promoted to a verified hit"
        );
        assert!(result.unattributed_positive_read);
        assert_eq!(scorecard.misses(), 1);
        Ok(())
    }

    #[test]
    fn estimated_cache_read_never_impersonates_provider_confirmation() -> TestResult {
        let usage = usage(
            Some(3),
            UsageField::estimated(
                TokenCount::new(7)?,
                PolicyVersion::new("estimate-1").ok_or("policy")?,
            ),
            UsageField::ABSENT,
        )?;
        let mut scorecard = CacheScorecard::new();
        let result = scorecard.observe(
            CacheMeasurementBounds::new(4, 2)?,
            attempt(&usage, PrefixTruth::Verified(verified("same")?))?,
        )?;
        assert!(
            !result.verified_hit,
            "an estimated cache-read leaf impersonated provider confirmation"
        );
        assert!(result.unattributed_positive_read);
        Ok(())
    }

    #[test]
    fn missing_is_not_zero_and_corrupt_entry_refuses_without_mutation() -> TestResult {
        let missing = usage(Some(4), UsageField::ABSENT, UsageField::ABSENT)?;
        let mut scorecard = CacheScorecard::new();
        let classification = scorecard.observe(
            CacheMeasurementBounds::new(4, 2)?,
            attempt(&missing, PrefixTruth::Missing)?,
        )?;
        assert_eq!(
            classification.cache_read,
            CacheReadClass::Unavailable,
            "a missing cache-read observation was reported as an observed zero"
        );
        let before = scorecard.clone();
        let error = scorecard.observe(
            CacheMeasurementBounds::new(4, 2)?,
            attempt(&missing, PrefixTruth::Corrupt(PrefixReceiptError::Empty))?,
        );
        assert_eq!(
            error,
            Err(CacheMeasurementError::CorruptPrefix(
                PrefixReceiptError::Empty
            )),
            "a corrupt prefix entry did not return its corruption failure before scorecard mutation"
        );
        assert_eq!(scorecard, before);
        Ok(())
    }

    #[test]
    fn attempt_profile_and_utf8_label_bounds_refuse() -> TestResult {
        let usage = usage(None, UsageField::ABSENT, UsageField::ABSENT)?;
        let mut scorecard = CacheScorecard::new();
        scorecard.observe(
            CacheMeasurementBounds::new(1, 1)?,
            attempt(&usage, PrefixTruth::Missing)?,
        )?;
        let before_attempt_bound = serde_json::to_vec(&scorecard)?;
        assert!(
            matches!(
                scorecard.observe(
                    CacheMeasurementBounds::new(1, 1)?,
                    attempt(&usage, PrefixTruth::Missing)?
                ),
                Err(CacheMeasurementError::AttemptBound { .. })
            ),
            "the cache scorecard admitted an attempt beyond its injected bound"
        );
        assert_eq!(
            serde_json::to_vec(&scorecard)?,
            before_attempt_bound,
            "attempt-bound refusal mutated counters or profiles"
        );

        let before_profile_bound = serde_json::to_vec(&scorecard)?;
        let profile_refusal = scorecard.observe(
            CacheMeasurementBounds::new(2, 1)?,
            CacheAttempt {
                profile: CacheProfile::new("another-profile")?,
                routing: RoutingKeyTruth::Present,
                prefix: PrefixTruth::Missing,
                usage: &usage,
                cost: CostTruth::Unavailable,
            },
        );
        assert_eq!(
            profile_refusal,
            Err(CacheMeasurementError::ProfileBound {
                limit: 1,
                offered: 2,
            })
        );
        assert_eq!(
            serde_json::to_vec(&scorecard)?,
            before_profile_bound,
            "profile-bound refusal inserted a label or changed counters"
        );
        assert_eq!(
            CacheProfile::new("é".repeat((MAX_CACHE_PROFILE_BYTES / 2) + 1)),
            Err(CacheProfileError::TooLong {
                maximum: MAX_CACHE_PROFILE_BYTES,
                offered: MAX_CACHE_PROFILE_BYTES + 2
            })
        );
        Ok(())
    }

    #[test]
    fn exact_checked_totals_do_not_saturate() {
        assert_eq!(
            add_u64(u64::MAX, 1, "witness"),
            Err(CacheMeasurementError::ArithmeticOverflow { field: "witness" }),
            "an overflowing cache token total saturated instead of refusing"
        );
        assert_eq!(
            add_u32(u32::MAX, 1, "witness"),
            Err(CacheMeasurementError::ArithmeticOverflow { field: "witness" })
        );
    }

    #[test]
    fn derived_usage_remains_measurable_but_not_reported() -> TestResult {
        let usage = usage(
            Some(3),
            UsageField::derived(
                TokenCount::new(7)?,
                UsageDerivation::FreshFromInclusiveInput,
            ),
            UsageField::ABSENT,
        )?;
        let mut scorecard = CacheScorecard::new();
        let result = scorecard.observe(
            CacheMeasurementBounds::new(4, 2)?,
            attempt(&usage, PrefixTruth::Verified(verified("same")?))?,
        )?;
        assert_eq!(result.cache_read, CacheReadClass::Positive);
        assert!(!result.verified_hit);
        Ok(())
    }

    #[derive(Debug, Deserialize)]
    struct CorpusAction {
        id: String,
        routing: String,
        prefix: String,
        cache_read: Option<u64>,
        provenance: String,
        cache_write: Option<u64>,
        cost: String,
        expect: String,
        eligibility: String,
        read: String,
        cost_class: String,
    }

    fn eligibility_tag(value: EligibilityClass) -> &'static str {
        match value {
            EligibilityClass::Reusable => "reusable",
            EligibilityClass::FirstObservation => "first-observation",
            EligibilityClass::Invalidated => "invalidated",
            EligibilityClass::Fieldless => "fieldless",
            EligibilityClass::Missing => "missing",
        }
    }

    #[test]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn language_neutral_acceptance_corpus_executes_every_truth_class() -> TestResult {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/rust-reconstruction/cache-usage-product-v1/actions.jsonl");
        let text = std::fs::read_to_string(path)?;
        let bounds = CacheMeasurementBounds::new(16, 2)?;
        let estimate_policy = PolicyVersion::new("price-2026-09").ok_or("policy")?;
        let mut scorecard = CacheScorecard::new();
        let mut refused = 0u32;
        for line in text.lines() {
            let action: CorpusAction = serde_json::from_str(line)?;
            assert!(action.id.len() <= MAX_CACHE_PROFILE_BYTES);
            let read = match (action.cache_read, action.provenance.as_str()) {
                (None, _) => UsageField::ABSENT,
                (Some(value), "reported") => UsageField::reported(TokenCount::new(value)?),
                (Some(value), "derived") => UsageField::derived(
                    TokenCount::new(value)?,
                    UsageDerivation::FreshFromInclusiveInput,
                ),
                (Some(value), "estimated") => {
                    UsageField::estimated(TokenCount::new(value)?, estimate_policy.clone())
                }
                (_, other) => {
                    return Err(format!("{} has unknown provenance {other}", action.id).into())
                }
            };
            let write = match action.cache_write {
                Some(value) => UsageField::reported(TokenCount::new(value)?),
                None => UsageField::ABSENT,
            };
            let usage = usage(Some(3), read, write)?;
            let routing = match action.routing.as_str() {
                "present" => RoutingKeyTruth::Present,
                "absent" => RoutingKeyTruth::Absent,
                "unsupported" => RoutingKeyTruth::Unsupported,
                other => return Err(format!("{} has unknown routing {other}", action.id).into()),
            };
            let prefix = match action.prefix.as_str() {
                "reusable" | "same" => PrefixTruth::Verified(verified("same")?),
                "invalidated" | "contribution-changed" => {
                    PrefixTruth::Verified(verified("contribution-changed")?)
                }
                "order-changed" | "adapter-changed" | "protocol-changed" | "policy-changed"
                | "key-rotated" => PrefixTruth::Verified(verified(action.prefix.as_str())?),
                "first" => PrefixTruth::FirstObservation,
                "fieldless" => PrefixTruth::Fieldless,
                "missing" => PrefixTruth::Missing,
                "corrupt" => PrefixTruth::Corrupt(PrefixReceiptError::Empty),
                other => return Err(format!("{} has unknown prefix {other}", action.id).into()),
            };
            let cost = match action.cost.as_str() {
                "billed" => CostTruth::Billed(MoneyUsd::new(0.01)?),
                "estimated" => CostTruth::Estimated {
                    amount: MoneyUsd::new(0.02)?,
                    policy: estimate_policy.clone(),
                },
                "unavailable" => CostTruth::Unavailable,
                other => return Err(format!("{} has unknown cost {other}", action.id).into()),
            };
            let before = scorecard.clone();
            let observed = scorecard.observe(
                bounds,
                CacheAttempt {
                    profile: CacheProfile::new("corpus")?,
                    routing,
                    prefix,
                    usage: &usage,
                    cost,
                },
            );
            if action.expect == "refused" {
                refused = refused.checked_add(1).ok_or("refusal overflow")?;
                assert!(
                    matches!(observed, Err(CacheMeasurementError::CorruptPrefix(_))),
                    "{} did not refuse",
                    action.id
                );
                assert_eq!(
                    scorecard, before,
                    "{} mutated the scorecard before refusal",
                    action.id
                );
                continue;
            }
            let observed = observed?;
            assert_eq!(
                eligibility_tag(observed.eligibility),
                action.eligibility,
                "{} keyed prefix eligibility",
                action.id
            );
            assert_eq!(
                observed.verified_hit,
                action.expect == "hit",
                "{} hit classification; an absent provider routing coordinate was treated as a verified hit",
                action.id
            );
            assert_eq!(
                format!("{:?}", observed.cache_read).to_ascii_lowercase(),
                action.read,
                "{} read classification",
                action.id
            );
            assert_eq!(
                format!("{:?}", observed.cost).to_ascii_lowercase(),
                action.cost_class,
                "{} cost classification",
                action.id
            );
        }
        assert_eq!(refused, 1);
        assert_eq!(scorecard.attempts(), 16);
        assert_eq!(scorecard.verified_hits(), 1);
        assert_eq!(scorecard.unattributed_positive_reads(), 10);
        let profile = scorecard.profile("corpus").ok_or("profile")?;
        assert_eq!(profile.attempts(), 16);
        assert_eq!(profile.verified_hits(), 1);
        assert_eq!(profile.fresh_tokens(), 48);
        Ok(())
    }
}
