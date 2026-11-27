//! Cost coverage is independent of the cache input/read pair's coverage.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum CallCost {
    #[default]
    Unknown,
    Unreadable,
    Reported(CostEstimate),
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostEstimate {
    pub(crate) usd: Option<f64>,
    pub(crate) priced: u64,
    pub(crate) missing_usage: u64,
    pub(crate) missing_pricing: u64,
    pub(crate) errors: u64,
    pub(crate) overflow: bool,
}
impl CallCost {
    pub(crate) fn preserve_gaps(self, left: Self, right: Self) -> Self {
        match (self, left, right) {
            (_, Self::Unreadable, _) | (_, _, Self::Unreadable) => Self::Unreadable,
            (Self::Reported(mut latest), Self::Reported(left), Self::Reported(right)) => {
                latest.missing_usage = left.missing_usage.max(right.missing_usage);
                latest.missing_pricing = left.missing_pricing.max(right.missing_pricing);
                latest.errors = left.errors.max(right.errors);
                latest.overflow = left.overflow || right.overflow;
                Self::Reported(latest)
            }
            _ => Self::Unknown,
        }
    }
    pub(super) fn status(self, attempts: u64, lost: bool) -> String {
        let Self::Reported(cost) = self else {
            return match self {
                Self::Unreadable => "API cost unreadable · subscription unknown".into(),
                _ => "API cost unknown · subscription unknown".into(),
            };
        };
        let coverage = format!(
            "{}/{} priced; missing usage {}, pricing {}; errors {}",
            crate::format::count(cost.priced),
            crate::format::count(attempts),
            crate::format::count(cost.missing_usage),
            crate::format::count(cost.missing_pricing),
            crate::format::count(cost.errors)
        );
        let amount = if cost.overflow {
            "API cost overflow".to_owned()
        } else if let Some(usd) = cost.usd {
            let kind = if lost
                || cost.priced != attempts
                || cost.missing_usage != 0
                || cost.missing_pricing != 0
                || cost.errors != 0
            {
                "subtotal"
            } else {
                "estimated"
            };
            format!("{kind} API USD {}", super::super::usd_amount(usd))
        } else {
            "API cost unknown".to_owned()
        };
        format!("{amount} (capture; {coverage}; subscription unknown)")
    }
}
