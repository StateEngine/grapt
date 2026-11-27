use super::*;

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn every_checked_overflow_refuses_with_a_byte_identical_scorecard() -> TestResult {
    let empty = usage(None, UsageField::ABSENT, UsageField::ABSENT)?;
    let read = usage(
        None,
        UsageField::reported(TokenCount::new(1)?),
        UsageField::ABSENT,
    )?;
    let fresh = usage(Some(1), UsageField::ABSENT, UsageField::ABSENT)?;
    let write = usage(
        None,
        UsageField::ABSENT,
        UsageField::reported(TokenCount::new(1)?),
    )?;

    let mut attempts = CacheScorecard::new();
    attempts.attempts = u32::MAX;
    assert_atomic_overflow(
        attempts,
        &empty,
        PrefixTruth::Missing,
        CostTruth::Unavailable,
        "attempts",
    )?;

    let mut profile_attempts = CacheScorecard::new();
    profile_attempts.profiles.insert(
        "responses".to_owned(),
        CacheProfileScore {
            attempts: u32::MAX,
            ..CacheProfileScore::default()
        },
    );
    assert_atomic_overflow(
        profile_attempts,
        &empty,
        PrefixTruth::Missing,
        CostTruth::Unavailable,
        "profile-attempts",
    )?;

    let mut profile_hits = CacheScorecard::new();
    profile_hits.profiles.insert(
        "responses".to_owned(),
        CacheProfileScore {
            verified_hits: u32::MAX,
            ..CacheProfileScore::default()
        },
    );
    assert_atomic_overflow(
        profile_hits,
        &read,
        PrefixTruth::Verified(verified("same")?),
        CostTruth::Unavailable,
        "profile-verified-hits",
    )?;

    for (field, held, observed) in [
        (
            "profile-read-tokens",
            CacheProfileScore {
                read_tokens: u64::MAX,
                ..CacheProfileScore::default()
            },
            &read,
        ),
        (
            "profile-fresh-tokens",
            CacheProfileScore {
                fresh_tokens: u64::MAX,
                ..CacheProfileScore::default()
            },
            &fresh,
        ),
        (
            "profile-write-tokens",
            CacheProfileScore {
                write_tokens: u64::MAX,
                ..CacheProfileScore::default()
            },
            &write,
        ),
    ] {
        let mut scorecard = CacheScorecard::new();
        scorecard.profiles.insert("responses".to_owned(), held);
        assert_atomic_overflow(
            scorecard,
            observed,
            PrefixTruth::Missing,
            CostTruth::Unavailable,
            field,
        )?;
    }

    let mut verified_hits = CacheScorecard::new();
    verified_hits.verified_hits = u32::MAX;
    assert_atomic_overflow(
        verified_hits,
        &read,
        PrefixTruth::Verified(verified("same")?),
        CostTruth::Unavailable,
        "verified-hits",
    )?;

    let mut misses = CacheScorecard::new();
    misses.misses = u32::MAX;
    assert_atomic_overflow(
        misses,
        &empty,
        PrefixTruth::Missing,
        CostTruth::Unavailable,
        "misses",
    )?;

    let mut unattributed = CacheScorecard::new();
    unattributed.unattributed_positive_reads = u32::MAX;
    assert_atomic_overflow(
        unattributed,
        &read,
        PrefixTruth::Verified(verified("policy-changed")?),
        CostTruth::Unavailable,
        "unattributed-positive-reads",
    )?;

    let mut unavailable_reads = CacheScorecard::new();
    unavailable_reads.unavailable_reads = u32::MAX;
    assert_atomic_overflow(
        unavailable_reads,
        &empty,
        PrefixTruth::Missing,
        CostTruth::Billed(MoneyUsd::new(1.0)?),
        "unavailable-reads",
    )?;

    for (field, cost) in [
        ("billed-costs", CostTruth::Billed(MoneyUsd::new(1.0)?)),
        (
            "estimated-costs",
            CostTruth::Estimated {
                amount: MoneyUsd::new(1.0)?,
                policy: PolicyVersion::new("atomic-cost").ok_or("policy")?,
            },
        ),
        ("unavailable-costs", CostTruth::Unavailable),
    ] {
        let mut scorecard = CacheScorecard::new();
        match field {
            "billed-costs" => scorecard.billed_costs = u32::MAX,
            "estimated-costs" => scorecard.estimated_costs = u32::MAX,
            "unavailable-costs" => scorecard.unavailable_costs = u32::MAX,
            _ => return Err("unknown cost fixture".into()),
        }
        assert_atomic_overflow(scorecard, &read, PrefixTruth::Missing, cost, field)?;
    }
    Ok(())
}
