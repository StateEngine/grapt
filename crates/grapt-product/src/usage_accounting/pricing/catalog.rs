//! Imported public catalog rates are USD per million, not observed billing.
use super::{CostTruth, PricePolicy, ReconciledUsage};
use serde_json::{json, Value};

/// Price an exact model's reconciled usage through the existing estimator.
/// Unsupported or ambiguous catalog tiers refuse instead of using a cheaper band.
pub fn catalog_cost(
    provider: &str,
    model: &str,
    metadata: &Value,
    usage: &ReconciledUsage,
) -> Result<(CostTruth, Option<&'static str>), String> {
    let Some(base) = metadata.get("cost").filter(|value| value.is_object()) else {
        return Ok((CostTruth::Unavailable, Some("catalog-price-unavailable")));
    };
    let mut selected = base;
    let mut threshold = 0;
    if let Some(tiers) = base.get("tiers") {
        let tiers = tiers
            .as_array()
            .ok_or("catalog price tiers are not an array")?;
        if tiers.len() > 32 {
            return Err("catalog price exceeds 32 tiers".into());
        }
        let input = usage
            .input_total()
            .ok_or("tiered price needs inclusive input")?
            .count()
            .value();
        let mut seen = std::collections::BTreeSet::new();
        for tier in tiers {
            let band = &tier["tier"];
            let size = band["size"]
                .as_u64()
                .filter(|size| *size > 0)
                .ok_or("catalog price tier has no positive context threshold")?;
            if band["type"] != "context" || !seen.insert(size) {
                return Err("catalog price tier is unsupported or ambiguous".into());
            }
            if input > size && size > threshold {
                selected = tier;
                threshold = size;
            }
        }
    }
    let policy = json!({"schema":"secode.price-policy.v1","version":"imported-catalog-usd-per-million",
        "models":[{"provider":provider,"model":model,"output_basis":"includes-reasoning",
            "usd_per_million_fresh_input":selected.get("input"),
            "usd_per_million_cache_read":selected.get("cache_read"),
            "usd_per_million_cache_write":selected.get("cache_write"),
            "usd_per_million_output":selected.get("output")}]});
    PricePolicy::decode(&serde_json::to_vec(&policy).map_err(|error| error.to_string())?)?
        .estimate(provider, model, usage)
}

#[cfg(test)]
mod tests;
