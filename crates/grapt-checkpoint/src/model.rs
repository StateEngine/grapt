use grapt_types::{
    CaptureRefusalReason, CaptureSkipReason, CheckpointFidelity, CheckpointHeadId,
    CheckpointLaneId, CheckpointLineage, CheckpointOperationId, CheckpointProviderId,
    CheckpointProviderVersion, CheckpointRestoreCapabilityId, CheckpointRestorePlanId,
    PathIdentity, PermissionId, RecoveryAnchor, RestoreConsequences, RestorePrecondition,
    RestoreRefusalReceipt, Timestamp, UnusualRestoreCondition, VcsBaseline, WorkspaceCheckpointId,
    WorkspaceCheckpointRef, WorktreeIdentity,
};

pub const MAX_CHECKPOINT_LANES: usize = 32;
pub const MAX_CHECKPOINT_ID_BYTES: usize = 256;
pub const MAX_JOURNAL_BYTES: usize = 32 * 1024;
pub const MAX_RESTORE_PRECONDITIONS: usize = MAX_CHECKPOINT_LANES * 3 + 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RescueSupport {
    Supported,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderLane {
    id: CheckpointLaneId,
    capture: bool,
    restore: bool,
}

impl ProviderLane {
    pub fn new(id: CheckpointLaneId, capture: bool, restore: bool) -> Self {
        Self {
            id,
            capture,
            restore,
        }
    }

    pub fn id(&self) -> &CheckpointLaneId {
        &self.id
    }

    pub fn supports_capture(&self) -> bool {
        self.capture
    }

    pub fn supports_restore(&self) -> bool {
        self.restore
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderContract {
    provider: CheckpointProviderId,
    version: CheckpointProviderVersion,
    lanes: Vec<ProviderLane>,
    rescue: RescueSupport,
}

impl ProviderContract {
    pub fn new(
        provider: CheckpointProviderId,
        version: CheckpointProviderVersion,
        lanes: Vec<ProviderLane>,
        rescue: RescueSupport,
    ) -> Result<Self, CheckpointError> {
        if lanes.is_empty() || lanes.len() > MAX_CHECKPOINT_LANES {
            return Err(CheckpointError::InvalidProviderContract);
        }
        let mut seen = Vec::with_capacity(lanes.len());
        for lane in &lanes {
            validate_identity(lane.id().as_str())?;
            if seen.iter().any(|known| known == lane.id()) {
                return Err(CheckpointError::InvalidProviderContract);
            }
            seen.push(lane.id().clone());
        }
        validate_identity(provider.as_str())?;
        validate_identity(version.as_str())?;
        Ok(Self {
            provider,
            version,
            lanes,
            rescue,
        })
    }

    pub fn provider(&self) -> &CheckpointProviderId {
        &self.provider
    }

    pub fn version(&self) -> &CheckpointProviderVersion {
        &self.version
    }

    pub fn lanes(&self) -> &[ProviderLane] {
        &self.lanes
    }

    pub fn rescue_support(&self) -> RescueSupport {
        self.rescue
    }

    pub fn lane(&self, id: &CheckpointLaneId) -> Option<&ProviderLane> {
        self.lanes.iter().find(|lane| lane.id() == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiatedProvider {
    contract: ProviderContract,
    lanes: Vec<CheckpointLaneId>,
}

impl NegotiatedProvider {
    pub(crate) fn new(contract: ProviderContract, lanes: Vec<CheckpointLaneId>) -> Self {
        Self { contract, lanes }
    }

    pub fn contract(&self) -> &ProviderContract {
        &self.contract
    }

    pub fn lanes(&self) -> &[CheckpointLaneId] {
        &self.lanes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceObservation {
    worktree: WorktreeIdentity,
    baseline: VcsBaseline,
}

impl WorkspaceObservation {
    pub fn new(worktree: WorktreeIdentity, baseline: VcsBaseline) -> Self {
        Self { worktree, baseline }
    }

    pub fn worktree(&self) -> &WorktreeIdentity {
        &self.worktree
    }

    pub fn baseline(&self) -> &VcsBaseline {
        &self.baseline
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointSelector {
    Immutable(WorkspaceCheckpointRef),
    Head {
        provider: CheckpointProviderId,
        version: CheckpointProviderVersion,
        head: CheckpointHeadId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderCaptureOutcome {
    Captured {
        checkpoint: WorkspaceCheckpointId,
        lineage: CheckpointLineage,
        fidelity: CheckpointFidelity,
    },
    Skipped {
        reason: CaptureSkipReason,
    },
    Refused {
        reason: CaptureRefusalReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderResolveOutcome {
    Resolved { checkpoint: WorkspaceCheckpointId },
    Refused { reason: ProviderRefusal },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInspection {
    checkpoint: WorkspaceCheckpointId,
    lanes: Vec<CheckpointLaneId>,
    fidelity: CheckpointFidelity,
    lineage: CheckpointLineage,
}

impl ProviderInspection {
    pub fn new(
        checkpoint: WorkspaceCheckpointId,
        lanes: Vec<CheckpointLaneId>,
        fidelity: CheckpointFidelity,
        lineage: CheckpointLineage,
    ) -> Self {
        Self {
            checkpoint,
            lanes,
            fidelity,
            lineage,
        }
    }

    pub fn checkpoint(&self) -> &WorkspaceCheckpointId {
        &self.checkpoint
    }

    pub fn lanes(&self) -> &[CheckpointLaneId] {
        &self.lanes
    }

    pub fn fidelity(&self) -> &CheckpointFidelity {
        &self.fidelity
    }

    pub fn lineage(&self) -> &CheckpointLineage {
        &self.lineage
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointInspection {
    reference: WorkspaceCheckpointRef,
    provider: ProviderInspection,
}

impl CheckpointInspection {
    pub(crate) fn new(reference: WorkspaceCheckpointRef, provider: ProviderInspection) -> Self {
        Self {
            reference,
            provider,
        }
    }

    pub fn reference(&self) -> &WorkspaceCheckpointRef {
        &self.reference
    }

    pub fn provider(&self) -> &ProviderInspection {
        &self.provider
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderForkOutcome {
    Forked {
        checkpoint: WorkspaceCheckpointId,
        head: CheckpointHeadId,
    },
    Refused {
        reason: ProviderRefusal,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkReceipt {
    operation: CheckpointOperationId,
    parent: WorkspaceCheckpointRef,
    child: WorkspaceCheckpointRef,
    head: CheckpointHeadId,
}

impl ForkReceipt {
    pub(crate) fn new(
        operation: CheckpointOperationId,
        parent: WorkspaceCheckpointRef,
        child: WorkspaceCheckpointRef,
        head: CheckpointHeadId,
    ) -> Self {
        Self {
            operation,
            parent,
            child,
            head,
        }
    }

    pub fn operation(&self) -> &CheckpointOperationId {
        &self.operation
    }

    pub fn parent(&self) -> &WorkspaceCheckpointRef {
        &self.parent
    }

    pub fn child(&self) -> &WorkspaceCheckpointRef {
        &self.child
    }

    pub fn head(&self) -> &CheckpointHeadId {
        &self.head
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureReceipt {
    operation: CheckpointOperationId,
    before: WorkspaceObservation,
    after: WorkspaceObservation,
    outcome: grapt_types::CheckpointCaptureOutcome,
}

impl CaptureReceipt {
    pub(crate) fn new(
        operation: CheckpointOperationId,
        before: WorkspaceObservation,
        after: WorkspaceObservation,
        outcome: grapt_types::CheckpointCaptureOutcome,
    ) -> Self {
        Self {
            operation,
            before,
            after,
            outcome,
        }
    }

    pub fn operation(&self) -> &CheckpointOperationId {
        &self.operation
    }

    pub fn before(&self) -> &WorkspaceObservation {
        &self.before
    }

    pub fn after(&self) -> &WorkspaceObservation {
        &self.after
    }

    pub fn outcome(&self) -> &grapt_types::CheckpointCaptureOutcome {
        &self.outcome
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderPlan {
    consequences: RestoreConsequences,
    unusual: Vec<UnusualRestoreCondition>,
    preconditions: Vec<RestorePrecondition>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderPreconditionObservation {
    satisfied: Vec<RestorePrecondition>,
}

impl ProviderPreconditionObservation {
    pub fn new(satisfied: Vec<RestorePrecondition>) -> Result<Self, CheckpointError> {
        if satisfied.len() > MAX_RESTORE_PRECONDITIONS {
            return Err(CheckpointError::ProviderResultOverBound);
        }
        Ok(Self { satisfied })
    }

    pub fn into_satisfied(self) -> Vec<RestorePrecondition> {
        self.satisfied
    }
}

impl ProviderPlan {
    pub fn new(
        consequences: RestoreConsequences,
        unusual: Vec<UnusualRestoreCondition>,
        preconditions: Vec<RestorePrecondition>,
    ) -> Result<Self, CheckpointError> {
        if unusual.len() > MAX_CHECKPOINT_LANES || preconditions.len() > MAX_CHECKPOINT_LANES * 2 {
            return Err(CheckpointError::ProviderResultOverBound);
        }
        Ok(Self {
            consequences,
            unusual,
            preconditions,
        })
    }

    pub fn consequences(&self) -> &RestoreConsequences {
        &self.consequences
    }

    pub fn unusual(&self) -> &[UnusualRestoreCondition] {
        &self.unusual
    }

    pub fn preconditions(&self) -> &[RestorePrecondition] {
        &self.preconditions
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePlanRequest {
    operation: CheckpointOperationId,
    checkpoint: WorkspaceCheckpointRef,
    root: PathIdentity,
    lanes: Vec<CheckpointLaneId>,
    expires_at: Timestamp,
    enforcement: grapt_types::RealizedEnforcement,
}

impl RestorePlanRequest {
    pub fn new(
        operation: CheckpointOperationId,
        checkpoint: WorkspaceCheckpointRef,
        root: PathIdentity,
        lanes: Vec<CheckpointLaneId>,
        expires_at: Timestamp,
        enforcement: grapt_types::RealizedEnforcement,
    ) -> Result<Self, CheckpointError> {
        validate_lanes(&lanes)?;
        validate_path(&root)?;
        Ok(Self {
            operation,
            checkpoint,
            root,
            lanes: canonical_lanes(lanes),
            expires_at,
            enforcement,
        })
    }

    pub fn operation(&self) -> &CheckpointOperationId {
        &self.operation
    }

    pub fn checkpoint(&self) -> &WorkspaceCheckpointRef {
        &self.checkpoint
    }

    pub fn root(&self) -> &PathIdentity {
        &self.root
    }

    pub fn lanes(&self) -> &[CheckpointLaneId] {
        &self.lanes
    }

    pub fn expires_at(&self) -> Timestamp {
        self.expires_at
    }

    pub fn enforcement(&self) -> &grapt_types::RealizedEnforcement {
        &self.enforcement
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityEvidence {
    permission: PermissionId,
}

impl AuthorityEvidence {
    pub fn new(permission: PermissionId) -> Self {
        Self { permission }
    }

    pub fn permission(&self) -> &PermissionId {
        &self.permission
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreAuthorityDecision {
    Granted {
        capability: CheckpointRestoreCapabilityId,
        evidence: AuthorityEvidence,
    },
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderRefusal {
    Unavailable,
    VersionMismatch,
    WrongRoot,
    CheckpointUnknown,
    HeadUnknown,
    LaneUnknown,
    PreconditionsChanged,
    OperationUnknown,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderApplyReceipt {
    operation: CheckpointOperationId,
    consequences: RestoreConsequences,
}

impl ProviderApplyReceipt {
    pub fn new(operation: CheckpointOperationId, consequences: RestoreConsequences) -> Self {
        Self {
            operation,
            consequences,
        }
    }

    pub fn operation(&self) -> &CheckpointOperationId {
        &self.operation
    }

    pub fn consequences(&self) -> &RestoreConsequences {
        &self.consequences
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderApplyOutcome {
    Applied(ProviderApplyReceipt),
    Refused(ProviderRefusal),
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderOperationState {
    Applied(ProviderApplyReceipt),
    Refused(ProviderRefusal),
    Ambiguous,
    Missing,
}

#[derive(Debug)]
pub enum RestoreApplyOutcome {
    Applied(ReconcileReceipt),
    Refused(RestoreApplicationRefusal),
    Ambiguous { operation: CheckpointOperationId },
    Quarantined(ReconcileReceipt),
}

#[derive(Debug)]
pub enum RestoreApplicationRefusal {
    AuthorityRefused,
    AuthorityMismatch,
    RescueUnavailable(Box<CaptureReceipt>),
    Admission(Box<RestoreRefusalReceipt>),
    DuplicateApply,
    Provider(ProviderRefusal),
    ProviderReceiptMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileReceipt {
    operation: CheckpointOperationId,
    plan: CheckpointRestorePlanId,
    checkpoint: WorkspaceCheckpointRef,
    state: ReconciledState,
}

impl ReconcileReceipt {
    pub(crate) fn new(
        operation: CheckpointOperationId,
        plan: CheckpointRestorePlanId,
        checkpoint: WorkspaceCheckpointRef,
        state: ReconciledState,
    ) -> Self {
        Self {
            operation,
            plan,
            checkpoint,
            state,
        }
    }

    pub fn operation(&self) -> &CheckpointOperationId {
        &self.operation
    }

    pub fn plan(&self) -> &CheckpointRestorePlanId {
        &self.plan
    }

    pub fn checkpoint(&self) -> &WorkspaceCheckpointRef {
        &self.checkpoint
    }

    pub fn state(&self) -> &ReconciledState {
        &self.state
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconciledState {
    Applied,
    Refused(ProviderRefusal),
    Quarantined,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreRecoveryOutcome {
    Applied(ReconcileReceipt),
    Refused(ReconcileReceipt),
    Quarantined(ReconcileReceipt),
}

#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    #[error("checkpoint provider contract is invalid")]
    InvalidProviderContract,
    #[error("checkpoint identity exceeds the declared bound")]
    IdentityOverBound,
    #[error("checkpoint lane selection is empty or exceeds the declared bound")]
    LaneSelectionOutOfBounds,
    #[error("checkpoint lane is unknown or unsupported for this operation")]
    LaneUnavailable,
    #[error("workspace root is unresolved or exceeds the declared bound")]
    InvalidRoot,
    #[error("the requested provider or version does not match the active contract")]
    ProviderContractMismatch,
    #[error("the provider returned an invalid or over-bound result")]
    ProviderResultOverBound,
    #[error("the provider refused the operation")]
    Provider(ProviderRefusal),
    #[error("a restore plan could not be minted from the provider result")]
    InvalidRestorePlan,
    #[error("the provider cannot prove a rescue checkpoint before restore")]
    RescueUnsupported,
    #[error("restore lifecycle journal failure: {0}")]
    Journal(#[from] crate::journal::JournalError),
}

pub(crate) fn validate_identity(raw: &str) -> Result<(), CheckpointError> {
    if raw.is_empty() || raw.len() > MAX_CHECKPOINT_ID_BYTES {
        Err(CheckpointError::IdentityOverBound)
    } else {
        Ok(())
    }
}

pub(crate) fn validate_path(path: &PathIdentity) -> Result<(), CheckpointError> {
    match path.as_canonical() {
        Some(raw) if raw.len() <= 4096 => Ok(()),
        Some(_) | None => Err(CheckpointError::InvalidRoot),
    }
}

pub(crate) fn validate_lanes(lanes: &[CheckpointLaneId]) -> Result<(), CheckpointError> {
    if lanes.is_empty() || lanes.len() > MAX_CHECKPOINT_LANES {
        return Err(CheckpointError::LaneSelectionOutOfBounds);
    }
    for lane in lanes {
        validate_identity(lane.as_str())?;
    }
    Ok(())
}

pub(crate) fn canonical_lanes(mut lanes: Vec<CheckpointLaneId>) -> Vec<CheckpointLaneId> {
    lanes.sort();
    lanes.dedup();
    lanes
}

pub(crate) fn validate_checkpoint(
    reference: &WorkspaceCheckpointRef,
) -> Result<(), CheckpointError> {
    validate_identity(reference.provider().as_str())?;
    validate_identity(reference.version().as_str())?;
    validate_identity(reference.checkpoint().as_str())
}

pub(crate) fn validate_lineage(lineage: &CheckpointLineage) -> Result<(), CheckpointError> {
    match lineage {
        CheckpointLineage::Root => Ok(()),
        CheckpointLineage::ForkedFrom { checkpoint, head } => {
            validate_identity(checkpoint.as_str())?;
            validate_identity(head.as_str())
        }
    }
}

pub(crate) fn anchor_from_capture(
    id: grapt_types::RecoveryAnchorId,
    reference: WorkspaceCheckpointRef,
    before: &WorkspaceObservation,
    after: &WorkspaceObservation,
    lanes: Vec<CheckpointLaneId>,
    fidelity: CheckpointFidelity,
) -> Option<RecoveryAnchor> {
    RecoveryAnchor::new(
        id,
        reference,
        before.baseline().clone(),
        after.baseline().clone(),
        before.worktree().clone(),
        lanes,
        fidelity,
    )
}

pub(crate) fn root_matches(observation: &WorkspaceObservation, root: &PathIdentity) -> bool {
    observation.worktree().root() == root
}

pub(crate) fn capture_refusal_for_baseline(baseline: &VcsBaseline) -> Option<CaptureRefusalReason> {
    match baseline {
        VcsBaseline::GitUnborn { .. } => Some(CaptureRefusalReason::UnbornGitRoot),
        VcsBaseline::Unavailable { reason } => Some(CaptureRefusalReason::VcsUnavailable {
            reason: reason.clone(),
        }),
        VcsBaseline::Git { .. } | VcsBaseline::NotVersioned => None,
    }
}

pub(crate) fn checkpoint_reference(
    contract: &ProviderContract,
    checkpoint: WorkspaceCheckpointId,
) -> WorkspaceCheckpointRef {
    WorkspaceCheckpointRef::new(
        contract.provider().clone(),
        contract.version().clone(),
        checkpoint,
    )
}

pub(crate) fn validate_inspection(
    inspection: &ProviderInspection,
    expected: &WorkspaceCheckpointRef,
) -> Result<(), CheckpointError> {
    if inspection.checkpoint() != expected.checkpoint() {
        return Err(CheckpointError::ProviderResultOverBound);
    }
    validate_lanes(inspection.lanes())?;
    validate_lineage(inspection.lineage())
}
