//! A published failure needs no fallback invalidation; its public text is unchanged.
use super::execution::ExecutionRefusal;

#[derive(Debug)]
pub(super) enum ExecutionFailure {
    Published(String),
    Unpublished(String),
}

impl From<ExecutionRefusal> for ExecutionFailure {
    fn from(error: ExecutionRefusal) -> Self {
        let reason = error.to_string();
        match error {
            ExecutionRefusal::PublishedFailure(_) => Self::Published(reason),
            _ => Self::Unpublished(reason),
        }
    }
}

impl ExecutionFailure {
    pub(super) fn into_reason(self) -> String {
        match self {
            Self::Published(reason) | Self::Unpublished(reason) => reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_status_preserves_the_public_started_error_text() {
        let original = ExecutionRefusal::Run("provider refused".to_owned());
        let published = ExecutionRefusal::PublishedFailure("provider refused".to_owned());
        let expected = "live provider execution refused: provider refused";
        assert_eq!(original.to_string(), expected);
        assert_eq!(published.to_string(), expected);
        let original = ExecutionFailure::from(original);
        let published = ExecutionFailure::from(published);
        assert!(matches!(&original, ExecutionFailure::Unpublished(_)));
        assert!(matches!(&published, ExecutionFailure::Published(_)));
        assert_eq!(original.into_reason(), expected);
        assert_eq!(published.into_reason(), expected);
    }
}
