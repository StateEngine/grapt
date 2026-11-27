//! Refresh classification before human-readable details are rendered.
use std::fmt;

#[derive(Debug, PartialEq, Eq)]
pub(in crate::dogfood) enum RefreshError {
    EpochCapacity,
    Failed(String),
}

impl RefreshError {
    pub(in crate::dogfood) fn compaction_trigger(&self) -> Option<grapt_types::CompactionTrigger> {
        matches!(self, Self::EpochCapacity)
            .then_some(grapt_types::CompactionTrigger::StructuralBound)
    }

    pub(super) fn history(error: grapt_instruction::InstructionError) -> Self {
        match error {
            grapt_instruction::InstructionError::EntryBound {
                bound: "instruction epoch request bytes",
            } => Self::EpochCapacity,
            error => Self::Failed(error.to_string()),
        }
    }
}

impl fmt::Display for RefreshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EpochCapacity => super::EPOCH_CONTEXT_BOUND,
            Self::Failed(detail) => detail,
        })
    }
}

impl std::error::Error for RefreshError {}

impl From<String> for RefreshError {
    fn from(detail: String) -> Self {
        Self::Failed(detail)
    }
}

impl From<&str> for RefreshError {
    fn from(detail: &str) -> Self {
        Self::Failed(detail.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_typed_epoch_capacity_requests_compaction() {
        let error = RefreshError::history(grapt_instruction::InstructionError::EntryBound {
            bound: "instruction epoch request bytes",
        });
        assert_eq!(
            error.compaction_trigger(),
            Some(grapt_types::CompactionTrigger::StructuralBound)
        );
        assert_eq!(error.to_string(), super::super::EPOCH_CONTEXT_BOUND);
        for error in [
            RefreshError::from(super::super::EPOCH_CONTEXT_BOUND),
            RefreshError::history(grapt_instruction::InstructionError::EntryBound {
                bound: "instruction request history",
            }),
        ] {
            assert_eq!(error.compaction_trigger(), None);
        }
    }
}
