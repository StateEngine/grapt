//! Provider completion disposition. Law (foundational report row
//! `completion`/`terminality`; supersession matrix "Provider completion" row):
//! the model's own claim about whether it is finished is a CLOSED vocabulary,
//! kept separate from the provider's finish/error class. "No treating provider
//! `length` as ordinary idle. Provider completion ends a Step chain, not an
//! active objective."
//!
//! This is the type owned by F1A; the runtime that acts on it, and the provider
//! finish/error class it is kept separate from, are later crates.

/// What the model asserted about its own completion, separate from the provider
/// finish reason. A closed enum: there is no bare `"complete"`/`"length"` string
/// and no two-valued complete/continue collapse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderCompletionDisposition {
    /// The model asserted the work of this Step chain is complete.
    AssertedComplete,
    /// The model asserted it intends to continue.
    AssertedContinue,
    /// The response was cut off at the provider's output bound; the model made
    /// no completion assertion. Never treated as ordinary idle.
    Truncated,
    /// The provider ended the response without any completion assertion.
    Unstated,
}

impl ProviderCompletionDisposition {
    /// Whether the model itself asserted completion. Truncation and silence are
    /// never completion.
    pub fn asserted_complete(self) -> bool {
        matches!(self, ProviderCompletionDisposition::AssertedComplete)
    }
}

#[cfg(test)]
mod tests {
    use super::ProviderCompletionDisposition::*;

    #[test]
    fn only_asserted_complete_is_completion() {
        assert!(AssertedComplete.asserted_complete());
        assert!(!AssertedContinue.asserted_complete());
        assert!(!Truncated.asserted_complete());
        assert!(!Unstated.asserted_complete());
    }
}
