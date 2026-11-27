//! Replay restoration keeps its classification before rendering a diagnostic.
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayError {
    CompactionOversize,
    Invalid(&'static str),
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CompactionOversize => super::COMPACTION_OVERSIZE,
            Self::Invalid(detail) => detail,
        })
    }
}

impl std::error::Error for ReplayError {}

impl From<&'static str> for ReplayError {
    fn from(detail: &'static str) -> Self {
        Self::Invalid(detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_typed_compaction_bound_is_oversize() {
        let typed = ReplayError::CompactionOversize;
        let text = ReplayError::from(super::super::COMPACTION_OVERSIZE);
        assert_ne!(typed, text);
        assert_eq!(typed.to_string(), text.to_string());
    }
}
