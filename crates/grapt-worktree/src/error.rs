use std::fmt;

/// Whether a failed operation reached an attempted checkout-changing command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeEffect {
    /// Validation or read-only preparation failed before mutation was attempted.
    NotStarted,
    /// Mutation was attempted; inspect the preserved checkout before retrying.
    MayHaveChanged,
}

/// A worktree failure with an explicit effect classification, never guessed
/// from the diagnostic message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorktreeError {
    NotStarted(String),
    MayHaveChanged(String),
}

impl WorktreeError {
    pub const fn effect(&self) -> WorktreeEffect {
        match self {
            Self::NotStarted(_) => WorktreeEffect::NotStarted,
            Self::MayHaveChanged(_) => WorktreeEffect::MayHaveChanged,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::NotStarted(message) | Self::MayHaveChanged(message) => message,
        }
    }

    pub const fn may_have_changed(&self) -> bool {
        matches!(self.effect(), WorktreeEffect::MayHaveChanged)
    }

    pub(crate) fn with_context(self, context: &str) -> Self {
        match self {
            Self::NotStarted(message) => Self::NotStarted(format!("{context}: {message}")),
            Self::MayHaveChanged(message) => Self::MayHaveChanged(format!("{context}: {message}")),
        }
    }
}

impl fmt::Display for WorktreeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for WorktreeError {}
