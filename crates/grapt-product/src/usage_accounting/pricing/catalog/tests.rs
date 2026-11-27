use super::*;
use grapt_types::{AttemptUsage, TokenCount, UsageField, UsageObservationPhase};

fn usage(fresh: u64, write: u64, read: u64, output: u64) -> Result<ReconciledUsage, String> {
    let field = |value| {
        TokenCount::new(value)
            .map(UsageField::reported)
            .map_err(|error| error.to_string())
    };
    grapt_types::usage::reconcile_usage(&[AttemptUsage::observed(
        field(fresh + write + read)?,
        field(fresh)?,
        field(output)?,
        UsageField::ABSENT,
        field(read)?,
        field(write)?,
        UsageField::ABSENT,
        UsageObservationPhase::Terminal,
    )])
    .map_err(|error| error.to_string())
}

fn metadata() -> Value {
    json!({"id":"claude-haiku-5-5","cost":{"input":0.1,"output":0.5,"cache_read":0.01,"cache_write":0.125,
        "tiers":[{"tier":{"type":"context","size":100000},"input":0.5,"output":2.5,"cache_read":0.05,"cache_write":0.625}]}})
}

fn amount(metadata: &Value, usage: &ReconciledUsage) -> Result<f64, String> {
    match catalog_cost("anthropic", "claude-haiku-5-5", metadata, usage)? {
        (CostTruth::Estimated { amount, .. }, _) => Ok(amount.amount()),
        _ => Err("catalog estimate unavailable".into()),
    }
}

#[test]
fn actual_haiku_usage_reconciles_to_catalog_cost_without_billing_inference() -> Result<(), String> {
    let cost = amount(&metadata(), &usage(4, 3604, 0, 50)?)?
        + amount(&metadata(), &usage(2, 131, 3604, 24)?)?;
    assert!((cost - 0.000540515).abs() < 1e-12);
    Ok(())
}

#[test]
fn context_tiers_apply_above_the_threshold_and_never_guess_unsupported_bands() -> Result<(), String>
{
    assert!((amount(&metadata(), &usage(100000, 0, 0, 0)?)? - 0.01).abs() < 1e-12);
    assert!((amount(&metadata(), &usage(100001, 0, 0, 0)?)? - 0.0500005).abs() < 1e-12);
    let mut invalid = metadata();
    invalid["cost"]["tiers"][0]["tier"]["type"] = json!("unknown");
    assert!(catalog_cost(
        "anthropic",
        "claude-haiku-5-5",
        &invalid,
        &usage(4, 0, 0, 1)?
    )
    .is_err());
    invalid = metadata();
    invalid["cost"]["tiers"] = json!([invalid["cost"]["tiers"][0], invalid["cost"]["tiers"][0]]);
    assert!(catalog_cost(
        "anthropic",
        "claude-haiku-5-5",
        &invalid,
        &usage(4, 0, 0, 1)?
    )
    .is_err());
    Ok(())
}

#[test]
fn missing_and_invalid_catalog_rates_do_not_become_zero() -> Result<(), String> {
    let counts = usage(4, 0, 0, 1)?;
    for value in [json!({}), json!({"cost":{"input":0.1,"output":0.5}})] {
        assert!(matches!(
            catalog_cost("anthropic", "claude-haiku-5-5", &value, &counts)?,
            (CostTruth::Unavailable, _)
        ));
    }
    assert!(catalog_cost(
        "anthropic",
        "claude-haiku-5-5",
        &json!({"cost":{"input":-1}}),
        &counts
    )
    .is_err());
    let zero = json!({"cost":{"input":0,"output":0,"cache_read":0,"cache_write":0}});
    assert_eq!(
        amount(&zero, &counts)?,
        0.0,
        "explicit zero rates are a genuine zero estimate"
    );
    Ok(())
}
