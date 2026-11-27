//! Older captures can lack prices; malformed new cost data remains distinct.
use crate::cache::{CallCost, CostEstimate};
use serde_json::Value;

pub(super) fn parse(calls: &Value, attempts: u64) -> CallCost {
    let Some(cost) = calls.get("cost") else {
        return CallCost::Unknown;
    };
    if cost.as_str() == Some("unavailable") {
        return CallCost::Unknown;
    }
    match estimate(cost, attempts, calls) {
        Some(cost) => CallCost::Reported(cost),
        None => CallCost::Unreadable,
    }
}
fn estimate(cost: &Value, attempts: u64, calls: &Value) -> Option<CostEstimate> {
    let number = |key| {
        cost.get(key)?
            .as_u64()
            .filter(|n| *n <= grapt_api::u3a::SafeTokenCount::MAX)
    };
    let priced = number("priced")?;
    let missing_usage = number("missingUsage")?;
    let missing_pricing = number("missingPricing")?;
    let errors = number("errors")?;
    let overflow = cost.get("overflow")?.as_bool()?;
    let sum = priced
        .checked_add(missing_usage)?
        .checked_add(missing_pricing)?
        .checked_add(errors)?;
    if (!overflow && sum != attempts)
        || cost.get("scope")?.as_str()? != "observed-dispatch-api-equivalent"
        || cost.get("subscription")?.as_str()? != "unknown"
    {
        return None;
    }
    let usd = match cost.get("usd")? {
        Value::Null => None,
        value => Some(
            value
                .as_f64()
                .filter(|usd| usd.is_finite() && *usd >= 0.0)?,
        ),
    };
    if usd.is_some() != (priced > 0 && !overflow) {
        return None;
    }
    let status = if usd.is_none() {
        "unavailable"
    } else if missing_usage == 0
        && missing_pricing == 0
        && errors == 0
        && calls.get("lostAtLeast")?.as_u64()? == 0
    {
        "estimated"
    } else {
        "subtotal"
    };
    if cost.get("status")?.as_str()? != status
        || (usd.is_some() && cost.get("policy")?.as_str()?.is_empty())
    {
        return None;
    }
    Some(CostEstimate {
        usd,
        priced,
        missing_usage,
        missing_pricing,
        errors,
        overflow,
    })
}
