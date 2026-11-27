//! Explicit fixture/operator prices, never a provider billing claim.
use crate::cache_accounting::CostTruth;
use grapt_types::{MoneyUsd, PolicyVersion, ReconciledUsage};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MAX_POLICY_BYTES: usize = 65_536;
const MAX_MODELS: usize = 128;
mod catalog;
mod pinned;
pub use catalog::catalog_cost;
pub use pinned::{assistant as assistant_cost, pinned, pinned_digest};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PricePolicy {
    schema: String,
    version: String,
    models: Vec<ModelPrice>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ModelPrice {
    provider: String,
    model: String,
    /// Output includes reasoning: pricing reasoning separately would double charge.
    output_basis: OutputBasis,
    usd_per_million_fresh_input: Option<f64>,
    usd_per_million_cache_read: Option<f64>,
    usd_per_million_cache_write: Option<f64>,
    usd_per_million_output: Option<f64>,
    /// Refuse requests outside a pinned price band instead of underpricing them.
    input_token_limit: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum OutputBasis {
    IncludesReasoning,
}

impl PricePolicy {
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_POLICY_BYTES {
            return Err("price policy exceeds 65536 bytes".into());
        }
        let policy: Self = serde_json::from_slice(bytes)
            .map_err(|_| "price policy is not valid closed-schema JSON".to_owned())?;
        if policy.schema != "secode.price-policy.v1" {
            return Err("unsupported price policy schema".into());
        }
        identity(&policy.version)?;
        if policy.models.is_empty() || policy.models.len() > MAX_MODELS {
            return Err("price policy requires 1..128 model entries".into());
        }
        let mut seen = BTreeSet::new();
        for model in &policy.models {
            identity(&model.provider)?;
            identity(&model.model)?;
            if !seen.insert((&model.provider, &model.model)) {
                return Err("price policy has ambiguous duplicate model rates".into());
            }
            for rate in model.rates().into_iter().flatten() {
                if !rate.is_finite() || rate.is_sign_negative() {
                    return Err(
                        "price rates must be finite nonnegative USD per million tokens".into(),
                    );
                }
            }
        }
        Ok(policy)
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn estimate(
        &self,
        provider: &str,
        model: &str,
        usage: &ReconciledUsage,
    ) -> Result<(CostTruth, Option<&'static str>), String> {
        let Some(price) = self
            .models
            .iter()
            .find(|p| p.provider == provider && p.model == model)
        else {
            return Ok((CostTruth::Unavailable, Some("model-price-unavailable")));
        };
        if price.input_token_limit.is_some_and(|limit| {
            usage
                .input_total()
                .is_none_or(|field| field.count().value() > limit)
        }) {
            return Ok((CostTruth::Unavailable, Some("input-price-band-unavailable")));
        }
        let fields = [
            usage.input_fresh(),
            usage.cache_read(),
            usage.cache_write(),
            usage.output(),
        ];
        if fields.iter().any(|field| field.is_none()) {
            return Ok((
                CostTruth::Unavailable,
                Some("usage-coordinates-unavailable"),
            ));
        }
        if price.rates().iter().any(Option::is_none) {
            return Ok((CostTruth::Unavailable, Some("price-rate-unavailable")));
        }
        let mut total = MoneyUsd::ZERO;
        for (field, rate) in fields.into_iter().zip(price.rates()) {
            let field = field.ok_or("required usage coordinate disappeared")?;
            let rate = rate.ok_or("required price rate disappeared")?;
            let amount = MoneyUsd::new(field.count().value() as f64 * rate / 1_000_000.0)
                .map_err(|_| "estimated cost arithmetic overflow".to_owned())?;
            if amount == MoneyUsd::ZERO && field.count().value() != 0 && rate != 0.0 {
                return Err("estimated cost arithmetic underflow".into());
            }
            total = total
                .checked_add(amount)
                .map_err(|_| "estimated cost sum overflow".to_owned())?;
        }
        let policy =
            PolicyVersion::new(self.version.clone()).ok_or("invalid price policy version")?;
        Ok((
            CostTruth::Estimated {
                amount: total,
                policy,
            },
            None,
        ))
    }
}

impl ModelPrice {
    fn rates(&self) -> [Option<f64>; 4] {
        [
            self.usd_per_million_fresh_input,
            self.usd_per_million_cache_read,
            self.usd_per_million_cache_write,
            self.usd_per_million_output,
        ]
    }
}

fn identity(value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err("price identity must be 1..128 bytes without control characters".into());
    }
    Ok(())
}

/// Final cost coverage is stricter than an estimate over the observed subtotal.
pub fn final_usage(usage: &ReconciledUsage) -> bool {
    [
        usage.input_fresh(),
        usage.cache_read(),
        usage.cache_write(),
        usage.output(),
    ]
    .into_iter()
    .all(|field| {
        field.is_some_and(|field| {
            matches!(
                field.phase(),
                grapt_types::UsageObservationPhase::Terminal
                    | grapt_types::UsageObservationPhase::Reconciled
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_types::{AttemptUsage, TokenCount, UsageField, UsageObservationPhase};
    use serde_json::json;

    fn policy(rate: f64) -> Vec<u8> {
        serde_json::to_vec(&json!({"schema":"secode.price-policy.v1","version":"fixture-v1",
            "models":[{"provider":"fixture","model":"deterministic","output_basis":"includes-reasoning",
                "usd_per_million_fresh_input":rate,"usd_per_million_cache_read":rate/10.0,
                "usd_per_million_cache_write":rate*2.0,"usd_per_million_output":rate*3.0}]}))
            .unwrap_or_else(|e| panic!("fixture encoding: {e}"))
    }

    fn usage(fresh: Option<u64>) -> Result<ReconciledUsage, String> {
        let field = |count| {
            TokenCount::new(count)
                .map(UsageField::reported)
                .map_err(|e| e.to_string())
        };
        let fresh = fresh.map(field).transpose()?.unwrap_or(UsageField::ABSENT);
        let usage = AttemptUsage::observed(
            field(110)?,
            fresh,
            field(10)?,
            field(4)?,
            field(7)?,
            field(3)?,
            field(120)?,
            UsageObservationPhase::Terminal,
        );
        grapt_types::usage::reconcile_usage(&[usage]).map_err(|e| e.to_string())
    }

    #[test]
    fn lua_estimate_prices_disjoint_coordinates_and_never_claims_billing() -> Result<(), String> {
        let policy = PricePolicy::decode(&policy(1.0))?;
        let (truth, reason) = policy.estimate("fixture", "deterministic", &usage(Some(100))?)?;
        assert_eq!(reason, None);
        match truth {
            CostTruth::Estimated { amount, policy } => {
                assert!(
                    (amount.amount() - 0.0001367).abs() < 1e-15,
                    "estimate charged a usage coordinate twice"
                );
                assert_eq!(policy.as_str(), "fixture-v1");
            }
            _ => panic!("calculated price must remain estimated"),
        }
        Ok(())
    }

    #[test]
    fn lua_missing_usage_and_price_are_unavailable_but_zero_rate_is_real() -> Result<(), String> {
        let prices = PricePolicy::decode(&policy(0.0))?;
        let (truth, reason) = prices.estimate("fixture", "deterministic", &usage(None)?)?;
        assert_eq!(truth, CostTruth::Unavailable);
        assert_eq!(reason, Some("usage-coordinates-unavailable"));
        assert_eq!(
            prices.estimate("fixture", "absent", &usage(Some(100))?)?.1,
            Some("model-price-unavailable")
        );
        assert!(
            matches!(prices.estimate("fixture","deterministic",&usage(Some(100))?)?.0,
            CostTruth::Estimated {amount,..} if amount==MoneyUsd::ZERO)
        );
        Ok(())
    }

    #[test]
    fn lua_price_input_refuses_negative_duplicate_unbounded_and_overflow() -> Result<(), String> {
        assert!(PricePolicy::decode(&policy(-1.0)).is_err());
        assert!(PricePolicy::decode(&vec![b' '; MAX_POLICY_BYTES + 1]).is_err());
        let mut value: serde_json::Value =
            serde_json::from_slice(&policy(1.0)).map_err(|e| e.to_string())?;
        let duplicate = value["models"][0].clone();
        value["models"]
            .as_array_mut()
            .ok_or("models")?
            .push(duplicate);
        assert!(
            PricePolicy::decode(&serde_json::to_vec(&value).map_err(|e| e.to_string())?).is_err()
        );
        let mut value: serde_json::Value =
            serde_json::from_slice(&policy(1.0)).map_err(|e| e.to_string())?;
        value["models"][0]["usd_per_million_fresh_input"] = json!(f64::MAX);
        let prices = PricePolicy::decode(&serde_json::to_vec(&value).map_err(|e| e.to_string())?)?;
        assert!(prices
            .estimate("fixture", "deterministic", &usage(Some(100))?)
            .is_err());
        Ok(())
    }

    #[test]
    fn lua_missing_rate_is_not_free_and_repeated_phases_do_not_add_charge() -> Result<(), String> {
        let mut value: serde_json::Value =
            serde_json::from_slice(&policy(1.0)).map_err(|e| e.to_string())?;
        value["models"][0]
            .as_object_mut()
            .ok_or("model")?
            .remove("usd_per_million_output");
        let missing = PricePolicy::decode(&serde_json::to_vec(&value).map_err(|e| e.to_string())?)?;
        assert_eq!(
            missing
                .estimate("fixture", "deterministic", &usage(Some(100))?)?
                .1,
            Some("price-rate-unavailable")
        );
        let field = |count| {
            TokenCount::new(count)
                .map(UsageField::reported)
                .map_err(|e| e.to_string())
        };
        let mut observation = AttemptUsage::observed(
            field(0)?,
            field(0)?,
            field(0)?,
            field(0)?,
            field(0)?,
            field(0)?,
            field(0)?,
            UsageObservationPhase::DuringStream,
        );
        let stream = observation.clone();
        observation.phase = UsageObservationPhase::Terminal;
        let reconciled =
            grapt_types::usage::reconcile_usage(&[stream, observation.clone(), observation])
                .map_err(|e| e.to_string())?;
        assert!(
            matches!(PricePolicy::decode(&policy(1.0))?.estimate("fixture","deterministic",&reconciled)?.0,
            CostTruth::Estimated {amount,..} if amount==MoneyUsd::ZERO)
        );
        Ok(())
    }
}
