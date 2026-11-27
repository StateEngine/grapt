//! API-equivalent estimates for this capture, never subscription charges.
use crate::{cache_accounting::CostTruth, usage_accounting::pricing};
use grapt_types::{ModelRef, MoneyUsd, ReconciledUsage};

pub(super) struct Totals {
    usd: MoneyUsd,
    priced: u64,
    missing_usage: u64,
    missing_pricing: u64,
    errors: u64,
    overflow: bool,
}
impl Default for Totals {
    fn default() -> Self {
        Self {
            usd: MoneyUsd::ZERO,
            priced: 0,
            missing_usage: 0,
            missing_pricing: 0,
            errors: 0,
            overflow: false,
        }
    }
}
impl Totals {
    pub(super) fn observe(&mut self, model: Option<&ModelRef>, usage: Option<&ReconciledUsage>) {
        let Some(usage) = usage.filter(|usage| pricing::final_usage(usage)) else {
            super::add(&mut self.missing_usage, 1, &mut self.overflow);
            return;
        };
        let Some(model) = model else {
            super::add(&mut self.missing_pricing, 1, &mut self.overflow);
            return;
        };
        let estimate = pricing::pinned().and_then(|policy| {
            policy.estimate(model.provider().as_str(), model.model().as_str(), usage)
        });
        match estimate {
            Ok((CostTruth::Estimated { amount, .. }, _)) => {
                super::add(&mut self.priced, 1, &mut self.overflow);
                match self.usd.checked_add(amount) {
                    Ok(usd) => self.usd = usd,
                    Err(_) => self.overflow = true,
                }
            }
            Ok(_) => super::add(&mut self.missing_pricing, 1, &mut self.overflow),
            Err(_) => super::add(&mut self.errors, 1, &mut self.overflow),
        }
    }

    pub(super) fn metadata(&self, lost: u64, overflow: bool) -> serde_json::Value {
        let overflow = overflow || self.overflow;
        let usd = (self.priced > 0 && !overflow).then(|| self.usd.amount());
        let status = if usd.is_none() {
            "unavailable"
        } else if self.missing_usage == 0
            && self.missing_pricing == 0
            && self.errors == 0
            && lost == 0
        {
            "estimated"
        } else {
            "subtotal"
        };
        serde_json::json!({
            "status":status, "usd":usd, "priced":self.priced,
            "missingUsage":self.missing_usage, "missingPricing":self.missing_pricing,
            "errors":self.errors, "overflow":overflow,
            "policy":pricing::pinned().ok().map(|policy| policy.version()),
            "scope":"observed-dispatch-api-equivalent", "subscription":"unknown",
        })
    }
}

#[cfg(test)]
mod tests;
