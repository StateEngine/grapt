use serde_json::Value;
use std::process::Command;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn million_attempt_benchmark_executes_keyed_comparisons_for_exact_hit_and_miss_halves() -> TestResult
{
    let output = Command::new(env!("CARGO_BIN_EXE_cache-usage-benchmark")).output()?;
    assert!(
        output.status.success(),
        "benchmark refused: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(receipt["iterations"], 1_000_000);
    assert_eq!(receipt["stable_attempts"], 500_000);
    assert_eq!(receipt["cold_attempts"], 500_000);
    assert_eq!(receipt["scorecard"]["attempts"], 1_000_000);
    assert_eq!(
        receipt["scorecard"]["verified_hits"], 500_000,
        "cold prefix comparisons were promoted to verified hits"
    );
    assert_eq!(receipt["scorecard"]["misses"], 500_000);
    assert_eq!(
        receipt["scorecard"]["profiles"]["benchmark-responses"]["read_tokens"],
        800_000_000
    );
    assert_eq!(
        receipt["scorecard"]["profiles"]["benchmark-responses"]["fresh_tokens"],
        200_000_000
    );
    Ok(())
}
