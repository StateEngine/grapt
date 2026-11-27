use super::*;

/// A named version for tests that build their own policy. Tests must supply
/// a real identity too: `PolicyLimit` names the policy that fired, so a
/// test whose policy had no identity would prove nothing about that field.
fn test_version() -> PolicyVersion {
    match PolicyVersion::new("test-policy/1") {
        Some(version) => version,
        None => panic!("a non-empty literal must build a PolicyVersion"),
    }
}

/// This crate's own ruled default policy, unwrapped for tests.
fn ruled() -> StepPolicy {
    match StepPolicy::ruled_default() {
        Ok(policy) => policy,
        Err(error) => panic!("the crate's ruled default policy must be valid: {error}"),
    }
}

#[test]
fn backoff_doubles_from_the_base() {
    let policy = ruled();
    assert_eq!(policy.backoff_for(1), Duration::from_secs(2));
    assert_eq!(policy.backoff_for(2), Duration::from_secs(4));
    assert_eq!(policy.backoff_for(3), Duration::from_secs(8));
}

#[test]
fn a_larger_provider_retry_after_wins() {
    let policy = ruled();
    let long = ProviderFailureClass::RateLimit {
        retry_after: Some(Duration::from_secs(30)),
    };
    assert_eq!(policy.delay_for(1, &long), Duration::from_secs(30));
    let hostile = ProviderFailureClass::RateLimit {
        retry_after: Some(Duration::from_secs(120)),
    };
    assert_eq!(policy.delay_for(1, &hostile), RATE_LIMIT_MAX_RETRY_DELAY);
    let short = ProviderFailureClass::RateLimit {
        retry_after: Some(Duration::from_millis(1)),
    };
    assert_eq!(policy.delay_for(1, &short), Duration::from_secs(2));
}

#[test]
fn the_default_is_the_oracle_policy() {
    assert_eq!(ruled().max_attempts(), 5);
    assert_eq!(ruled().max_overflow_rebuilds(), 1);
}

#[test]
fn invalid_policies_are_rejected_at_construction() {
    let ok = Duration::from_secs(1);
    assert_eq!(
        StepPolicy::new(test_version(), 0, ok, 1, ok),
        Err(PolicyError::NoAttempts)
    );
    assert_eq!(
        StepPolicy::new(test_version(), 5, ok, 2, ok),
        Err(PolicyError::TooManyRebuilds { requested: 2 })
    );
    assert_eq!(
        StepPolicy::new(test_version(), 5, ok, 1, Duration::ZERO),
        Err(PolicyError::ZeroTimeout)
    );
    assert!(
        StepPolicy::new(
            test_version(),
            1,
            Duration::ZERO,
            0,
            Duration::from_millis(1)
        )
        .is_ok(),
        "short durations and zero backoff stay constructible for tests"
    );
}

#[test]
fn a_generation_advance_is_checked_not_saturating() {
    let first = HistoryGeneration::new(7);
    match first.next() {
        Some(next) => {
            assert_eq!(next.value(), 8);
            assert!(next > first);
        }
        None => panic!("7 must advance"),
    }
    // At the ceiling it refuses rather than standing still. A saturating
    // advance would let a rebuild look like it observed new history.
    assert_eq!(HistoryGeneration::new(u64::MAX).next(), None);
}

#[tokio::test]
async fn no_compaction_refuses_rather_than_pretending() {
    let mut history = NoCompaction;
    assert!(history.compact().await.is_err());
    assert!(history.reload().is_err());
}

#[test]
fn a_policy_over_the_ruled_cap_is_rejected() {
    let ok = Duration::from_secs(1);
    assert_eq!(
        StepPolicy::new(test_version(), MAX_TOTAL_ATTEMPTS + 1, ok, 1, ok),
        Err(PolicyError::TooManyAttempts {
            requested: MAX_TOTAL_ATTEMPTS + 1
        })
    );
    assert_eq!(
        StepPolicy::new(test_version(), u32::MAX, ok, 1, ok),
        Err(PolicyError::TooManyAttempts {
            requested: u32::MAX
        }),
        "an unbounded attempt count is the defect the cap exists to prevent"
    );
    assert!(StepPolicy::new(test_version(), MAX_TOTAL_ATTEMPTS, ok, 1, ok).is_ok());
}

#[test]
fn jittered_backoff_is_deterministic_bounded_and_varies_by_salt() {
    let step = StepNumber::new(3).unwrap_or_else(|| panic!("valid step"));
    let base = Duration::from_secs(10);
    let ceiling = Duration::from_secs(60);
    let salt_a = backoff::salt_from_str("pat_session_a_1");
    let salt_b = backoff::salt_from_str("pat_session_b_1");
    let first = backoff::jittered(base, step, 1, salt_a, ceiling);
    let second = backoff::jittered(base, step, 1, salt_a, ceiling);
    let other = backoff::jittered(base, step, 1, salt_b, ceiling);
    assert_eq!(first, second);
    assert_ne!(first, other);
    assert!(first >= base && first <= Duration::from_millis(12_500));
    assert_eq!(
        backoff::jittered(Duration::ZERO, step, 1, salt_a, ceiling),
        Duration::ZERO
    );
    assert_eq!(
        backoff::jittered(Duration::from_secs(60), step, 2, salt_a, ceiling),
        ceiling
    );
}
