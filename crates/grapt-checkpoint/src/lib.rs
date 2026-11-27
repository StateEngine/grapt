//! External workspace-checkpoint application boundary.
//!
//! This crate coordinates a versioned external provider, an independent VCS
//! observer, explicit restore authority, and a durable application journal. It
//! never stores checkpoint payloads and never asks the provider to move Git
//! HEAD, refs, or the index.

mod engine;
mod journal;
mod model;
mod ports;

pub use engine::CheckpointEngine;
pub use journal::{FileRestoreJournal, JournalError};
pub use model::{
    AuthorityEvidence, CaptureReceipt, CheckpointError, CheckpointInspection, CheckpointSelector,
    ForkReceipt, NegotiatedProvider, ProviderApplyOutcome, ProviderApplyReceipt,
    ProviderCaptureOutcome, ProviderContract, ProviderForkOutcome, ProviderInspection,
    ProviderLane, ProviderOperationState, ProviderPlan, ProviderPreconditionObservation,
    ProviderRefusal, ProviderResolveOutcome, ReconcileReceipt, ReconciledState, RescueSupport,
    RestoreApplicationRefusal, RestoreApplyOutcome, RestoreAuthorityDecision, RestorePlanRequest,
    RestoreRecoveryOutcome, WorkspaceObservation, MAX_CHECKPOINT_ID_BYTES, MAX_CHECKPOINT_LANES,
    MAX_JOURNAL_BYTES, MAX_RESTORE_PRECONDITIONS,
};
pub use ports::{CheckpointProvider, RestoreAuthorityPort, VcsObserver};

pub use grapt_types::{
    CaptureRefusalReason, CaptureSkipReason, CheckpointFidelity, CheckpointLaneId,
    CheckpointLineage, CheckpointOperationId, CheckpointProviderId, CheckpointProviderVersion,
    CheckpointRestorePlanId, RecoveryAnchor, RestoreConsequences, RestorePrecondition,
    SettledRestorePlan, UnusualRestoreCondition, WorkspaceCheckpointId, WorkspaceCheckpointRef,
};

#[cfg(test)]
mod tests;
