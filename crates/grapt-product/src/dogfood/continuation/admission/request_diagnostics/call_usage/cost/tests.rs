use super::*;
use grapt_types::*;
fn model(provider: &str, name: &str) -> Result<ModelRef, String> {
    let provider = ProviderId::new(provider).ok_or("provider")?;
    let model = ModelId::new(name).ok_or("model")?;
    let catalog = ModelCatalog::load(
        ModelCatalogVersion::new("cost-fixture").ok_or("version")?,
        vec![ModelCatalogRecord::new(
            provider.clone(),
            model.clone(),
            BaseModelId::new(name).ok_or("base")?,
            ModelFamilyId::new("cost-fixture").ok_or("family")?,
            None,
        )],
    )
    .ok_or("catalog")?;
    Ok(ModelRef::from_catalog(
        catalog.entry(&provider, &model, None).ok_or("entry")?,
    ))
}
fn usage(output: Option<u64>, phase: UsageObservationPhase) -> Result<ReconciledUsage, String> {
    let field = |n| {
        TokenCount::new(n)
            .map(UsageField::reported)
            .map_err(|e| e.to_string())
    };
    let row = AttemptUsage::observed(
        field(1100)?,
        field(1000)?,
        output.map(field).transpose()?.unwrap_or(UsageField::ABSENT),
        field(4)?,
        field(80)?,
        field(20)?,
        UsageField::ABSENT,
        phase,
    );
    usage::reconcile_usage(&[row]).map_err(|e| e.to_string())
}
#[test]
fn exact_models_are_priced_once_with_missing_coordinates_and_prices_distinct() -> Result<(), String>
{
    let mut totals = Totals::default();
    let sol = model("openai", "gpt-6.1-sol")?;
    let claude = model("anthropic", "claude-opus-5-5")?;
    let counts = usage(Some(10), UsageObservationPhase::Terminal)?;
    totals.observe(Some(&sol), Some(&counts));
    totals.observe(Some(&claude), Some(&counts));
    let complete = totals.metadata(0, false);
    assert_eq!(complete["status"], "estimated");
    assert_eq!(complete["priced"], 2);
    assert!((complete["usd"].as_f64().ok_or("usd")? - 0.006534).abs() < 1e-12);
    assert_eq!(complete["subscription"], "unknown");
    totals.observe(Some(&model("custom", "gpt-6.1-sol")?), Some(&counts));
    totals.observe(None, Some(&counts));
    totals.observe(Some(&sol), None);
    totals.observe(
        Some(&sol),
        Some(&usage(None, UsageObservationPhase::Terminal)?),
    );
    totals.observe(
        Some(&sol),
        Some(&usage(Some(10), UsageObservationPhase::DuringStream)?),
    );
    let partial = totals.metadata(0, false);
    assert_eq!(partial["status"], "subtotal");
    assert_eq!(partial["missingUsage"], 3);
    assert_eq!(partial["missingPricing"], 2);
    assert_eq!(partial["usd"], complete["usd"]);
    Ok(())
}
#[test]
fn missing_lost_and_overflowed_cost_is_never_a_complete_or_free_capture() -> Result<(), String> {
    let mut totals = Totals::default();
    assert!(totals.metadata(0, false)["usd"].is_null());
    totals.observe(Some(&model("openai", "gpt-6-luna")?), None);
    assert_eq!(totals.metadata(0, false)["status"], "unavailable");
    totals.observe(
        Some(&model("openai", "gpt-6-luna")?),
        Some(&usage(Some(0), UsageObservationPhase::Terminal)?),
    );
    assert_eq!(totals.metadata(2, false)["status"], "subtotal");
    assert!(totals.metadata(0, true)["usd"].is_null());
    Ok(())
}
