// Copyright 2025 OpenAI. Apache-2.0.
// Modified by Grapt: read existing durable AssistantSelection facts; no new event owner.
//! Reconstruct generation controls before complete originating input turns.
use grapt_provider::{CompactionTarget, ReasoningEffort};
use std::collections::BTreeMap;

pub(super) fn updates(
    users: &[grapt_inbox::PromotedMessage],
    messages: &[grapt_session::history::AssistantMessage],
    ceiling: u64,
    target: Option<&CompactionTarget>,
) -> BTreeMap<u64, ReasoningEffort> {
    let Some(target) = target.filter(|target| target.effort_policy().updates()) else {
        return BTreeMap::new();
    };
    let policy = target.effort_policy();
    let Some(current) = policy.resolve(target.model().variant()) else {
        return BTreeMap::new();
    };
    let mut result = BTreeMap::new();
    let mut established = None;
    for (index, user) in users.iter().enumerate() {
        let until = users.get(index + 1).map_or(ceiling, |next| next.seq);
        let mut observed = None;
        let mut incompatible = false;
        for message in messages
            .iter()
            .filter(|message| message.opened_at_seq > user.seq && message.opened_at_seq < until)
        {
            let effort = message.selection.as_ref().and_then(|selection| {
                (selection.provider() == target.model().provider()
                    && selection.model() == target.model().model())
                // Known discovery defaults are resolved by the existing target
                // issuer and recorded as variants. An absent historical variant
                // has no known value; today's default cannot supply yesterday's.
                .then(|| {
                    selection
                        .variant()
                        .and_then(|variant| ReasoningEffort::parse(variant.as_str()))
                })
                .flatten()
            });
            if effort.is_none() || observed.is_some_and(|previous| Some(previous) != effort) {
                incompatible = true;
            }
            observed = effort;
        }
        let effort = if index + 1 == users.len() {
            // A setting change during a tool continuation cannot take effect
            // through a next-input control. Use ordinary request-level effort.
            if incompatible || observed.is_some_and(|previous| previous != current) {
                return BTreeMap::new();
            }
            Some(current)
        } else if incompatible {
            None
        } else {
            observed
        };
        let Some(effort) = effort else {
            // A different model, unknown selection or abandoned input starts
            // a new prefix regime; never invent a historical default.
            result.clear();
            established = None;
            continue;
        };
        if established != Some(effort) {
            result.insert(user.seq, effort);
            established = Some(effort);
        }
    }
    result
}

impl super::DurableContinuation {
    /// The target the next request is planned for.
    #[cfg(all(test, feature = "loopback-provider-testkit"))]
    pub(in crate::dogfood) fn select_target(
        &self,
        target: &grapt_types::ResolvedModelTarget,
        family: grapt_provider::ProtocolFamily,
    ) {
        self.select_target_with_effort(target, family, grapt_provider::EffortPolicy::default());
    }

    pub(in crate::dogfood) fn select_target_with_effort(
        &self,
        target: &grapt_types::ResolvedModelTarget,
        family: grapt_provider::ProtocolFamily,
        effort: grapt_provider::EffortPolicy,
    ) {
        *super::lock(&self.redeemer) =
            Some(grapt_provider::CompactionTarget::of(target, family).with_effort_policy(effort));
    }
}
