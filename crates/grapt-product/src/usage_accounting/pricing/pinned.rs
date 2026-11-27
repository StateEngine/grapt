//! Standard API-equivalent rates; subscription charges and billing are unreported.
//! Sources and cache-duration assumptions are documented in CACHE_SEAT_DAY.md.
use super::PricePolicy;
use std::sync::OnceLock;
const BYTES: &[u8] = include_bytes!("api-2026-10-27.json");
static POLICY: OnceLock<Result<PricePolicy, String>> = OnceLock::new();

pub fn pinned() -> Result<&'static PricePolicy, String> {
    POLICY
        .get_or_init(|| PricePolicy::decode(BYTES))
        .as_ref()
        .map_err(Clone::clone)
}

pub fn pinned_digest() -> String {
    ring::digest::digest(&ring::digest::SHA256, BYTES)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Price only the exact settled attempt represented by this public assistant row.
/// Retried and compaction attempts are accounted for by the durable usage report.
pub fn assistant(
    message: &grapt_session::history::AssistantMessage,
) -> Result<serde_json::Value, String> {
    use crate::cache_accounting::CostTruth;
    use serde_json::json;
    let unavailable = |reason| json!({"status":"unavailable", "reason":reason});
    if message.usage_attempt != message.attempt {
        return Ok(unavailable("exact-attempt-model-unavailable"));
    }
    if !message
        .usage
        .iter()
        .any(|usage| usage.phase() == grapt_types::UsageObservationPhase::Terminal)
    {
        return Ok(unavailable("terminal-usage-unavailable"));
    }
    let Some(selection) = &message.selection else {
        return Ok(unavailable("exact-attempt-model-unavailable"));
    };
    let usage = grapt_types::usage::reconcile_recorded_usage(&message.usage)
        .map_err(|error| error.to_string())?;
    if !super::final_usage(&usage) {
        return Ok(unavailable("terminal-usage-unavailable"));
    }
    match pinned()?.estimate(
        selection.provider().as_str(),
        selection.model().as_str(),
        &usage,
    )? {
        (CostTruth::Estimated { amount, policy }, _) => {
            Ok(json!({"status":"estimated", "usd":amount.amount(),
            "policy":policy.as_str(), "scope":"assistant-message-api-equivalent"}))
        }
        (_, reason) => Ok(unavailable(reason.unwrap_or("cost-unavailable"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache_accounting::CostTruth;
    use grapt_types::{
        AttemptUsage, ReconciledUsage, TokenCount, UsageField, UsageObservationPhase,
    };
    fn usage(input: u64, output: Option<u64>) -> Result<ReconciledUsage, String> {
        let field = |n| {
            TokenCount::new(n)
                .map(UsageField::reported)
                .map_err(|e| e.to_string())
        };
        let observation = AttemptUsage::observed(
            field(input + 100)?,
            field(input)?,
            output.map(field).transpose()?.unwrap_or(UsageField::ABSENT),
            field(0)?,
            field(80)?,
            field(20)?,
            UsageField::ABSENT,
            UsageObservationPhase::Terminal,
        );
        grapt_types::usage::reconcile_usage(&[observation]).map_err(|e| e.to_string())
    }
    #[test]
    fn pinned_cost_uses_exact_models_native_cache_rates_and_closed_input_band() -> Result<(), String>
    {
        let policy = pinned()?;
        for (provider, model, expected) in [
            ("openai", "gpt-6.1-sol", 0.002158),
            ("openai", "gpt-6-luna", 0.0001083),
            ("anthropic", "claude-opus-5-5", 0.004376),
        ] {
            let (cost, reason) = policy.estimate(provider, model, &usage(1000, Some(10))?)?;
            assert_eq!(reason, None);
            match cost {
                CostTruth::Estimated { amount, .. } => {
                    assert!((amount.amount() - expected).abs() < 1e-12)
                }
                _ => return Err("pinned estimate absent".into()),
            }
        }
        for (provider, model, counts, reason) in [
            (
                "custom",
                "gpt-6.1-sol",
                usage(1000, Some(10))?,
                "model-price-unavailable",
            ),
            (
                "openai",
                "sol",
                usage(1000, Some(10))?,
                "model-price-unavailable",
            ),
            (
                "openai",
                "gpt-6.1-sol",
                usage(272000, Some(10))?,
                "input-price-band-unavailable",
            ),
            (
                "anthropic",
                "claude-opus-5-5",
                usage(1000, None)?,
                "usage-coordinates-unavailable",
            ),
        ] {
            let (cost, why) = policy.estimate(provider, model, &counts)?;
            assert_eq!(cost, CostTruth::Unavailable);
            assert_eq!(why, Some(reason));
        }
        Ok(())
    }
}
