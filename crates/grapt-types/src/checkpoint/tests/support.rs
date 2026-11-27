use super::super::*;
pub(super) use crate::authority::test_support::{FixedClock, FixedIds};
pub(super) use crate::path::PathIdentity;
pub(super) use crate::vcs::{BranchName, BranchState, GitCommitId, VcsBaseline};

pub(super) fn checkpoint_ref() -> Option<WorkspaceCheckpointRef> {
    Some(WorkspaceCheckpointRef::new(
        CheckpointProviderId::new("stateengine")?,
        CheckpointProviderVersion::new("1.2.0")?,
        WorkspaceCheckpointId::new("ckpt-1")?,
    ))
}

pub(super) fn baseline() -> Option<VcsBaseline> {
    Some(VcsBaseline::Git {
        head: GitCommitId::new("0123456789abcdef0123456789abcdef01234567")?,
        branch: BranchState::Symbolic(BranchName::new("main")?),
    })
}

pub(super) fn anchor_with(fidelity: CheckpointFidelity) -> Option<RecoveryAnchor> {
    RecoveryAnchor::new(
        crate::recovery::RecoveryAnchorId::new("anchor-1")?,
        checkpoint_ref()?,
        baseline()?,
        baseline()?,
        WorktreeIdentity::Plain {
            root: PathIdentity::canonical("/work"),
        },
        vec![CheckpointLaneId::new("files")?],
        fidelity,
    )
}

pub(super) fn anchor() -> Option<RecoveryAnchor> {
    anchor_with(CheckpointFidelity::Exact)
}

pub(super) const SETTLED_MS: i64 = 1_700_000_000_000;
pub(super) const TTL_MS: i64 = 60_000;

pub(super) fn work_root() -> WorktreeIdentity {
    WorktreeIdentity::Plain {
        root: PathIdentity::canonical("/work"),
    }
}

pub(super) fn settled_plan_with(
    plan_id: &str,
    root: WorktreeIdentity,
    lanes: Vec<CheckpointLaneId>,
    unusual: Vec<UnusualRestoreCondition>,
    expires_at_ms: i64,
    now_ms: i64,
) -> Option<SettledRestorePlan> {
    let clock = FixedClock(now_ms);
    let ids = FixedIds;
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    SettledRestorePlan::settle_at_provider_boundary(
        SettledRestorePlanParts {
            plan: CheckpointRestorePlanId::new(plan_id)?,
            operation: CheckpointOperationId::new("op-1")?,
            checkpoint: checkpoint_ref()?,
            root,
            lanes,
            consequences: RestoreConsequences::new(12, 3, 4096),
            unusual,
            preconditions: vec![RestorePrecondition::WorktreeQuiescent],
            enforcement: RealizedEnforcement::HarnessOnly,
            expires_at: Timestamp::from_millis(expires_at_ms),
        },
        &authorities,
    )
}

pub(super) fn settled_plan() -> Option<SettledRestorePlan> {
    settled_plan_with(
        "plan-1",
        work_root(),
        vec![CheckpointLaneId::new("files")?],
        Vec::new(),
        SETTLED_MS + TTL_MS,
        SETTLED_MS,
    )
}

/// An anchor for a rescue capture: a *different* checkpoint (the state
/// about to be overwritten) on the same worktree the plan writes.
pub(super) fn rescue_anchor_for(
    root: WorktreeIdentity,
    checkpoint: &str,
) -> Option<RecoveryAnchor> {
    RecoveryAnchor::new(
        RecoveryAnchorId::new("anchor-rescue")?,
        WorkspaceCheckpointRef::new(
            CheckpointProviderId::new("stateengine")?,
            CheckpointProviderVersion::new("1.2.0")?,
            WorkspaceCheckpointId::new(checkpoint)?,
        ),
        baseline()?,
        baseline()?,
        root,
        vec![CheckpointLaneId::new("files")?],
        CheckpointFidelity::Exact,
    )
}

pub(super) fn rescue_for(plan: &SettledRestorePlan) -> Option<RestoreRescue> {
    let anchor = rescue_anchor_for(plan.root().clone(), "ckpt-rescue")?;
    RestoreRescue::captured_at_provider_boundary(plan, &anchor)
}

pub(super) fn lease_for(
    capability: &CheckpointRestoreCapabilityId,
) -> Option<CheckpointRestoreAuthorityLease> {
    Some(CheckpointRestoreAuthorityLease::issue_at_boundary(
        capability.clone(),
    ))
}

/// Observe every precondition the plan requires, at `at_ms`.
pub(super) fn observation_for(
    plan: &SettledRestorePlan,
    at_ms: i64,
) -> PlanPreconditionObservation {
    let clock = FixedClock(at_ms);
    let ids = FixedIds;
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    PlanPreconditionObservation::observe_at_provider_boundary(
        plan,
        plan.preconditions().to_vec(),
        &authorities,
    )
}

pub(super) fn apply_at(
    plan: SettledRestorePlan,
    rescue: RestoreRescue,
    lease: CheckpointRestoreAuthorityLease,
    now_ms: i64,
) -> RestoreAdmission {
    let observation = observation_for(&plan, SETTLED_MS);
    apply_with(plan, rescue, observation, lease, now_ms)
}

pub(super) fn apply_with(
    plan: SettledRestorePlan,
    rescue: RestoreRescue,
    preconditions: PlanPreconditionObservation,
    lease: CheckpointRestoreAuthorityLease,
    now_ms: i64,
) -> RestoreAdmission {
    let clock = FixedClock(now_ms);
    let ids = FixedIds;
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    plan.apply(rescue, preconditions, lease, &authorities)
}
