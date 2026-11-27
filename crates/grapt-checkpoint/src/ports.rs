use crate::model::{
    CheckpointSelector, ProviderApplyOutcome, ProviderCaptureOutcome, ProviderContract,
    ProviderForkOutcome, ProviderInspection, ProviderOperationState, ProviderPlan,
    ProviderPreconditionObservation, ProviderResolveOutcome, RestoreAuthorityDecision,
    WorkspaceObservation,
};
use grapt_types::{
    CheckpointHeadId, CheckpointLaneId, CheckpointOperationId, PathIdentity,
    RestoreApplicationAdmission, SettledRestorePlan, WorkspaceCheckpointRef, WorktreeIdentity,
};

/// Independent workspace/VCS observation port. A provider never implements
/// this port: Git and checkpoint storage remain separate engines.
pub trait VcsObserver: core::fmt::Debug + Send {
    fn observe(&mut self, root: &PathIdentity) -> WorkspaceObservation;
}

/// External provider contract. Checkpoint content stays behind this boundary;
/// request and receipt types carry only identities and bounded counts.
pub trait CheckpointProvider: core::fmt::Debug + Send {
    fn contract(&self) -> Result<ProviderContract, crate::CheckpointError>;

    fn capture(
        &mut self,
        operation: &CheckpointOperationId,
        root: &WorktreeIdentity,
        lanes: &[CheckpointLaneId],
    ) -> ProviderCaptureOutcome;

    fn resolve(&mut self, selector: &CheckpointSelector) -> ProviderResolveOutcome;

    fn inspect(
        &mut self,
        checkpoint: &WorkspaceCheckpointRef,
    ) -> Result<ProviderInspection, crate::ProviderRefusal>;

    fn fork(
        &mut self,
        operation: &CheckpointOperationId,
        parent: &WorkspaceCheckpointRef,
        head: &CheckpointHeadId,
    ) -> ProviderForkOutcome;

    fn plan_restore(
        &mut self,
        operation: &CheckpointOperationId,
        checkpoint: &WorkspaceCheckpointRef,
        root: &WorktreeIdentity,
        lanes: &[CheckpointLaneId],
    ) -> Result<ProviderPlan, crate::ProviderRefusal>;

    fn observe_preconditions(
        &mut self,
        plan: &SettledRestorePlan,
    ) -> Result<ProviderPreconditionObservation, crate::ProviderRefusal>;

    fn apply_restore(&mut self, admission: RestoreApplicationAdmission) -> ProviderApplyOutcome;

    fn inspect_operation(&mut self, operation: &CheckpointOperationId) -> ProviderOperationState;

    fn reconcile_operation(&mut self, operation: &CheckpointOperationId) -> ProviderOperationState;
}

/// Explicit authority is consulted for the exact derived restore capability.
/// The port may return another plan's live decision; the engine compares it
/// and refuses rather than treating "some restore approval" as this approval.
pub trait RestoreAuthorityPort: core::fmt::Debug + Send {
    fn decide(&mut self, plan: &SettledRestorePlan) -> RestoreAuthorityDecision;
}
