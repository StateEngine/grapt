// Copyright 2025 OpenAI. Apache-2.0.
// Modified by Grapt: reconstruct trusted selections without a second durable writer.
//! Closed generation settings; never changes target or replay credential authority.
use crate::{CodexInputItem, ProviderRequest};
use grapt_types::{ModelRef, ModelVariantId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}
impl ReasoningEffort {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "none" => Self::None,
            "minimal" => Self::Minimal,
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            "xhigh" => Self::Xhigh,
            "max" => Self::Max,
            _ => return Option::None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
    pub fn native(self) -> bool {
        !matches!(self, Self::None | Self::Minimal)
    }
}

/// How discovery permits this model's generation setting to change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EffortMode {
    #[default]
    Unavailable,
    Request,
    Chronological,
}

/// Discovery policy for the selected model. Unknown defaults stay unknown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EffortPolicy {
    pub mode: EffortMode,
    pub default: Option<ReasoningEffort>,
}
impl EffortPolicy {
    pub fn updates(self) -> bool {
        self.mode == EffortMode::Chronological
    }

    pub fn resolve(self, variant: Option<&ModelVariantId>) -> Option<ReasoningEffort> {
        match variant {
            Some(variant) => ReasoningEffort::parse(variant.as_str()),
            None => self.default,
        }
    }
    /// Only closed effort values are compatible. Other variant identities
    /// still require equality; provider/model and credential checks remain separate.
    /// Provider-default is a generation setting even when its value is unknown;
    /// it does not establish a chronological baseline.
    pub(crate) fn variants_match(
        self,
        historical: Option<&ModelVariantId>,
        selected: Option<&ModelVariantId>,
    ) -> bool {
        let generation = |variant: Option<&ModelVariantId>| {
            variant.is_none_or(|variant| ReasoningEffort::parse(variant.as_str()).is_some())
        };
        historical == selected
            || (self.mode != EffortMode::Unavailable
                && generation(historical)
                && generation(selected))
    }
}

pub(crate) fn policy(request: &ProviderRequest) -> EffortPolicy {
    match request {
        ProviderRequest::ChatGptCodex(request) => request.reasoning.effort,
        ProviderRequest::AnthropicMessages(request) | ProviderRequest::VertexAnthropic(request) => {
            request.effort
        }
        _ => EffortPolicy::default(),
    }
}

pub fn supports_anthropic_effort_updates(
    provider: &grapt_types::ProviderId,
    model: &grapt_types::ModelId,
) -> bool {
    provider.as_str() == "anthropic"
        && matches!(
            model.as_str(),
            "claude-fable-5-1"
                | "claude-mythos-5-1"
                | "claude-opus-5-5"
                | "claude-opus-5"
                | "claude-sonnet-5-5"
                | "claude-haiku-5-5"
        )
}

pub(crate) struct Projection {
    pub baseline: Option<ReasoningEffort>,
    pub updates: bool,
}
// Upstream copyright and MIT license: see THIRD_PARTY_NOTICES.md.
// Baseline/last-override agreement.
/// Reverts/forks must agree with the latest trusted override before freezing
/// the baseline. Unsupported routes retain their ordinary top-level setting.
pub(crate) fn projection(
    items: &[CodexInputItem],
    model: &ModelRef,
    policy: EffortPolicy,
) -> Projection {
    let current = policy.resolve(model.variant());
    let mut updates = items.iter().filter_map(|item| match item {
        CodexInputItem::EffortUpdate(effort) => Some(*effort),
        _ => None,
    });
    let first = updates.next();
    let last = updates.next_back().or(first);
    let enabled = policy.updates() && current.is_some() && last == current;
    Projection {
        baseline: if enabled { first } else { current },
        updates: enabled,
    }
}
