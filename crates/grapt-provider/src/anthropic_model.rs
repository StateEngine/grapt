// Upstream copyright and MIT license: see THIRD_PARTY_NOTICES.md.
// Modified by Grapt: exact documented aliases replace the upstream version heuristic.
// Original source: the oracle's `anthropic-messages.ts`,
//! Native System support is a protocol property, not model issuance authority.

use grapt_types::ModelRef;

/// Whether the native Claude API accepts chronological System text for this model.
/// Unknown aliases keep the legacy prefix path. Sonnet 5 is explicitly unsupported.
/// https://platform.claude.com/docs/en/build-with-claude/mid-conversation-system-messages
pub fn supports_anthropic_system_updates(model: &ModelRef) -> bool {
    named(model.provider().as_str(), model.model().as_str())
}

fn named(provider: &str, model: &str) -> bool {
    provider == "anthropic"
        && matches!(
            model,
            "claude-opus-5-5"
                | "claude-opus-4-8"
                | "claude-opus-5"
                | "claude-sonnet-5-5"
                | "claude-haiku-5-5"
                | "claude-fable-5-1"
                | "claude-mythos-5-1"
                | "claude-fable-5"
                | "claude-mythos-5"
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn support_is_not_inferred_from_a_future_version_or_another_provider() {
        for model in ["claude-opus-5-5", "claude-opus-4-8", "claude-sonnet-5-5"] {
            assert!(named("anthropic", model));
            assert!(!named("other", model));
        }
        for model in [
            "claude-sonnet-5",
            "claude-opus-4-7",
            "claude-opus-6",
            "claude-live",
        ] {
            assert!(!named("anthropic", model));
        }
    }
}
