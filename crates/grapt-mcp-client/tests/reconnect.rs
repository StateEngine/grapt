use grapt_mcp_client::{ReconnectExhausted, ReconnectPolicy};
use std::error::Error;
use std::time::Duration;

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn rapid_disconnects_back_off_and_exhaust_a_finite_budget() -> TestResult {
    let mut policy = ReconnectPolicy::new();
    let expected = [0, 500, 1000, 2000, 4000, 8000, 16000, 30000];
    for milliseconds in expected {
        assert_eq!(
            policy.next_delay(Duration::ZERO, None)?,
            Duration::from_millis(milliseconds)
        );
    }
    for _ in 0..8 {
        assert_eq!(
            policy.next_delay(Duration::ZERO, None)?,
            Duration::from_secs(30)
        );
    }
    for _ in 0..4 {
        assert_eq!(
            policy.next_delay(Duration::ZERO, None),
            Err(ReconnectExhausted)
        );
    }
    Ok(())
}

#[test]
fn healthy_stream_resets_the_outage_budget_without_a_clock() -> TestResult {
    let mut policy = ReconnectPolicy::new();
    for _ in 0..16 {
        policy.next_delay(Duration::ZERO, None)?;
    }
    assert_eq!(
        policy.next_delay(Duration::from_secs(2), None)?,
        Duration::ZERO
    );
    assert_eq!(
        policy.next_delay(Duration::ZERO, None)?,
        Duration::from_millis(500)
    );
    Ok(())
}

#[test]
fn server_retry_cannot_remove_backoff_or_create_unbounded_waits() -> TestResult {
    let mut policy = ReconnectPolicy::new();
    assert_eq!(
        policy.next_delay(Duration::ZERO, Some(Duration::from_secs(5)))?,
        Duration::from_secs(5)
    );
    assert_eq!(
        policy.next_delay(Duration::ZERO, Some(Duration::ZERO))?,
        Duration::from_millis(500)
    );
    assert_eq!(
        policy.next_delay(Duration::ZERO, Some(Duration::from_secs(120)))?,
        Duration::from_secs(120)
    );
    assert_eq!(
        policy.next_delay(Duration::ZERO, Some(Duration::MAX)),
        Err(ReconnectExhausted)
    );
    Ok(())
}
