//! A summary that cannot advance differs from an unreadable summary source.
use std::fmt;

#[derive(Debug, PartialEq, Eq)]
pub(in crate::dogfood) enum PreparationError {
    NoProgress(String),
    Failed(String),
}

impl PreparationError {
    pub(super) fn no_prefix() -> Self {
        Self::NoProgress("no complete history prefix fits the summary budget".to_owned())
    }

    pub(in crate::dogfood) fn into_failure(self) -> grapt_runtime::CompactionFailure {
        match self {
            Self::NoProgress(reason) => grapt_runtime::CompactionFailure::NoProgress { reason },
            Self::Failed(reason) => {
                grapt_runtime::CompactionFailure::Failed(grapt_runtime::CompactionError { reason })
            }
        }
    }
}

impl From<super::super::context_error::ContextError> for PreparationError {
    fn from(error: super::super::context_error::ContextError) -> Self {
        use super::super::context_error::ContextError;
        match error {
            capacity @ ContextError::Capacity(_) => Self::NoProgress(capacity.to_string()),
            ContextError::Failed(detail) => Self::Failed(detail),
        }
    }
}

impl From<String> for PreparationError {
    fn from(detail: String) -> Self {
        Self::Failed(detail)
    }
}

impl From<&str> for PreparationError {
    fn from(detail: &str) -> Self {
        Self::Failed(detail.to_owned())
    }
}

impl From<PreparationError> for String {
    fn from(error: PreparationError) -> Self {
        error.to_string()
    }
}

impl fmt::Display for PreparationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoProgress(reason) | Self::Failed(reason) => f.write_str(reason),
        }
    }
}

impl std::error::Error for PreparationError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_cannot_promote_a_source_failure_to_no_progress() {
        let error = PreparationError::no_prefix();
        let detail = error.to_string();
        assert!(matches!(
            error.into_failure(),
            grapt_runtime::CompactionFailure::NoProgress { .. }
        ));
        assert!(matches!(
            PreparationError::from(detail).into_failure(),
            grapt_runtime::CompactionFailure::Failed(_)
        ));
    }
}
