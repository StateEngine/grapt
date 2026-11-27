//! Product-authored compaction recovery copy, never provider diagnostic text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionRecovery {
    Context,
    Authentication,
    Quota,
    UsageLimit {
        reset: Option<grapt_types::provider_limit::ProviderLimitReset>,
    },
    Transient,
    Other,
    NoProgress,
    DidNotFit,
}

impl CompactionRecovery {
    pub fn message(self) -> String {
        match self {
            Self::Context => "Automatic compaction failed after a context overflow. Change the model or context budget, rewind, or request /compact to retry.",
            Self::Authentication => "Automatic compaction failed after an authentication failure. Sign in again, then send another prompt.",
            Self::Quota => "Automatic compaction failed after a credit or quota failure. Restore credit, then send another prompt.",
            Self::Transient => "Automatic compaction failed after a transient failure. Send another prompt or request /compact to retry.",
            Self::NoProgress => "Automatic compaction made no progress. Your history is intact; send another prompt to continue with a visibly trimmed context.",
            Self::DidNotFit => "The compacted context still did not fit the model's context window. Your history is intact; send another prompt to continue with a visibly trimmed context.",
            Self::Other => "Automatic compaction failed. Change the model or context budget, rewind, or request /compact to retry.",
            Self::UsageLimit { reset } => return format!(
                "Automatic compaction failed. {} Switch accounts, then send another prompt.",
                super::provider_limit_message(reset)
            ),
        }.to_owned()
    }
}
