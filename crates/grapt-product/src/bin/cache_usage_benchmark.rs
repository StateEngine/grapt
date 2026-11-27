use grapt_policy::{PrefixIdentityAuthority, PreparedPrefixPart, Secret};
use grapt_product::cache_accounting::{
    CacheAttempt, CacheMeasurementBounds, CacheProfile, CacheScorecard, CostTruth, PrefixTruth,
    RoutingKeyTruth,
};
use grapt_types::{
    reconcile_usage, AdapterVersion, AttemptUsage, ContextEpoch, ContributionId, PolicyVersion,
    PrefixIdentityBounds, PrefixKeyRealm, PrefixKeyVersion, ProtocolVersion,
    ProviderCacheNamespace, TokenCount, UsageField, UsageObservationPhase,
};
use serde::Serialize;

const BENCHMARK_ITERATIONS: u32 = 1_000_000;

#[derive(Serialize)]
struct BenchmarkReceipt {
    schema: &'static str,
    candidate_binding: &'static str,
    iterations: u32,
    stable_attempts: u32,
    cold_attempts: u32,
    scorecard: CacheScorecard,
}

fn main() {
    let code = match run() {
        Ok(receipt) => match serde_json::to_string(&receipt) {
            Ok(encoded) => {
                println!("{encoded}");
                0
            }
            Err(error) => refuse(&format!("receipt encoding failed: {error}")),
        },
        Err(error) => refuse(&error),
    };
    std::process::exit(code);
}

fn run() -> Result<BenchmarkReceipt, String> {
    let iterations = BENCHMARK_ITERATIONS;
    let stable_usage = usage(200, 800)?;
    // Both halves report the same positive provider cache read. Therefore the
    // exact 50/50 hit/miss split can only come from the keyed prefix
    // comparison; usage shape cannot make the cold half miss by itself.
    let cold_usage = usage(200, 800)?;
    let bounds = CacheMeasurementBounds::new(iterations, 1).map_err(|error| error.to_string())?;
    let profile = CacheProfile::new("benchmark-responses").map_err(|error| error.to_string())?;
    let authority = PrefixIdentityAuthority::new(
        PrefixKeyRealm::new("benchmark").map_err(|error| error.to_string())?,
        PrefixKeyVersion::new("v1").map_err(|error| error.to_string())?,
        Secret::new(b"benchmark-prefix-key".to_vec()),
    );
    let namespace = ProviderCacheNamespace::new("responses").map_err(|error| error.to_string())?;
    let adapter = AdapterVersion::new("benchmark-adapter-v1")
        .ok_or_else(|| "benchmark adapter version is empty".to_owned())?;
    let protocol = ProtocolVersion::new("benchmark-protocol-v1")
        .ok_or_else(|| "benchmark protocol version is empty".to_owned())?;
    let policy = PolicyVersion::new("benchmark-policy-v1")
        .ok_or_else(|| "benchmark policy version is empty".to_owned())?;
    let contribution = ContributionId::new("benchmark-system")
        .ok_or_else(|| "benchmark contribution identity is empty".to_owned())?;
    let committed_parts = [PreparedPrefixPart {
        contribution: &contribution,
        bytes: b"stable-prefix",
    }];
    let cold_parts = [PreparedPrefixPart {
        contribution: &contribution,
        bytes: b"cold-prefix",
    }];
    let receipt = authority
        .mint(
            namespace.clone(),
            ContextEpoch::new(1),
            adapter.clone(),
            protocol.clone(),
            policy.clone(),
            PrefixIdentityBounds::new(1, 32, 32).map_err(|error| error.to_string())?,
            &committed_parts,
        )
        .map_err(|error| error.to_string())?;
    let mut scorecard = CacheScorecard::new();
    let mut stable_attempts = 0u32;
    let mut cold_attempts = 0u32;
    for ordinal in 0..iterations {
        let stable = ordinal % 2 == 0;
        let usage = if stable { &stable_usage } else { &cold_usage };
        let offered_parts = if stable {
            stable_attempts = stable_attempts
                .checked_add(1)
                .ok_or_else(|| "stable attempt count overflowed".to_owned())?;
            &committed_parts[..]
        } else {
            cold_attempts = cold_attempts
                .checked_add(1)
                .ok_or_else(|| "cold attempt count overflowed".to_owned())?;
            &cold_parts[..]
        };
        let prefix = PrefixTruth::Verified(
            authority
                .compare_committed_prefix(
                    &receipt,
                    &namespace,
                    ContextEpoch::new(1),
                    &adapter,
                    &protocol,
                    &policy,
                    offered_parts,
                )
                .map_err(|error| error.to_string())?,
        );
        scorecard
            .observe(
                bounds,
                CacheAttempt {
                    profile: profile.clone(),
                    routing: RoutingKeyTruth::Present,
                    prefix,
                    usage,
                    cost: CostTruth::Unavailable,
                },
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(BenchmarkReceipt {
        schema: "grapt.cache.benchmark.v1",
        candidate_binding: "remote-compute-runner-receipt",
        iterations,
        stable_attempts,
        cold_attempts,
        scorecard,
    })
}

fn usage(fresh: u64, read: u64) -> Result<grapt_types::ReconciledUsage, String> {
    let fresh = TokenCount::new(fresh).map_err(|error| error.to_string())?;
    let read = TokenCount::new(read).map_err(|error| error.to_string())?;
    reconcile_usage(&[AttemptUsage::observed(
        UsageField::ABSENT,
        UsageField::reported(fresh),
        UsageField::ABSENT,
        UsageField::ABSENT,
        UsageField::reported(read),
        UsageField::reported(TokenCount::ZERO),
        UsageField::ABSENT,
        UsageObservationPhase::Terminal,
    )])
    .map_err(|error| error.to_string())
}

fn refuse(reason: &str) -> i32 {
    eprintln!("cache benchmark refused: {reason}");
    2
}
