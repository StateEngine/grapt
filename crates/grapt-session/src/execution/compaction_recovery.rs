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
    Capacity,
}

impl CompactionRecovery {
    pub fn message(self) -> String {
        match self {
            Self::Context => "automatic compaction paused after context overflow; change the model/context budget, rewind, or request /compact to retry",
            Self::Authentication => "automatic compaction paused after authentication failure; sign in again and request /compact to retry",
            Self::Quota => "automatic compaction paused after credit/quota failure; restore credit and request /compact, or retry after a successful model response",
            Self::Transient => "automatic compaction paused after a transient failure; the next user input or /compact may retry",
            Self::NoProgress => "Automatic compaction made no progress. Your history is intact; send another prompt to continue with a visibly trimmed context.",
            Self::Other => "automatic compaction paused after a non-transient failure; change the model/context budget, rewind, or request /compact to retry",
            Self::Capacity => "automatic compaction recovery capacity reached; request /compact to retry manually",
            Self::UsageLimit { reset } => return format!(
                "Automatic compaction paused. {} Request /compact after switching accounts.",
                super::provider_limit_message(reset)
            ),
        }.to_owned()
    }
}
