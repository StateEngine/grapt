use crate::state::InvalidTransition;
use crate::store::LoadError;
use grapt_run_store::WriteError;

/// Why a submit was not applied. Admission refusals leave the session
/// unchanged and can be retried with the same submit key.
#[derive(Debug, thiserror::Error)]
pub enum SubmitError {
    #[error("the session's command mailbox is full; retry with the same submit key")]
    MailboxFull,
    #[error("run storage is not admitting writes; retry with the same submit key")]
    StorageStalled,
    #[error("run storage reached its WAL budget; retry once readers release their snapshots")]
    WalBudgetExceeded,
    #[error(transparent)]
    InvalidTransition(#[from] InvalidTransition),
    #[error("submit key was already used for a different event in this session")]
    KeyConflict,
    #[error("the transition's outcome is unknown; retry with the same submit key")]
    OutcomeUnknown,
    #[error("an earlier transition's outcome is still unresolved; retry with the same submit key")]
    PriorUnsettled,
    #[error("another writer changed this session; its state is reloaded before the next submit")]
    Contended,
    #[error("the session's state sequence is exhausted")]
    SequenceExhausted,
    #[error("the session actor has stopped")]
    Closed,
    #[error(transparent)]
    Load(#[from] LoadError),
    #[error(transparent)]
    Storage(WriteError),
}

impl From<WriteError> for SubmitError {
    fn from(error: WriteError) -> Self {
        match error {
            WriteError::QueueFull | WriteError::ByteBudget | WriteError::NotAdmitted => {
                Self::StorageStalled
            }
            WriteError::WalPressure => Self::WalBudgetExceeded,
            WriteError::OutcomeUnknown { .. } => Self::OutcomeUnknown,
            WriteError::KeyConflict => Self::KeyConflict,
            WriteError::Closed => Self::Closed,
            error => Self::Storage(error),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("could not load the session's runtime state: {0}")]
    Load(#[from] LoadError),
    #[error("the session actor stopped abnormally")]
    Worker,
    #[error("the session actor stopped before a transition's outcome was known; resubmit it with the same submit key")]
    Unsettled,
}

#[cfg(test)]
mod tests {
    use super::SubmitError;
    use grapt_run_store::WriteError;

    #[test]
    fn storage_backpressure_maps_to_typed_refusals() {
        for stalled in [
            WriteError::QueueFull,
            WriteError::ByteBudget,
            WriteError::NotAdmitted,
        ] {
            assert!(matches!(
                SubmitError::from(stalled),
                SubmitError::StorageStalled
            ));
        }
        assert!(matches!(
            SubmitError::from(WriteError::WalPressure),
            SubmitError::WalBudgetExceeded
        ));
    }
}
