// Probe 42 (row 12, second clause): applying a different plan is unnameable.
//
// Law: "The restore plan binds provider/version, checkpoint, root identity,
// selected lanes, writes/deletes/bytes, unusual conditions and provider
// preconditions. Applying a different plan is unnameable."
//
// The old shape was `restore(checkpoint)` plus `apply_plan(plan_id)`: two
// independent calls whose arguments a caller chose separately, so the
// consequences that were planned and the checkpoint that was written did not
// have to be the same ones. The governed entry now reads every coordinate out of
// the settled plan it consumed. There is no checkpoint argument, no root
// argument and no lane argument to substitute — and neither a bare checkpoint
// reference nor a plan *identity* can stand in for the settled plan itself,
// because a settled plan is the whole authorized coordinate set, not a name for
// one.
//
// Error phase: type mismatch, resolved during type checking.
use grapt_types::{
    AuthorityLease, CheckpointRestorePlanId, DeterministicAuthorities,
    PlanPreconditionObservation, RestoreAdmission, RestoreRescue, SettledRestorePlan,
    WorkspaceCheckpointRef,
};

fn apply_a_bare_checkpoint(
    checkpoint: WorkspaceCheckpointRef,
    rescue: RestoreRescue,
    preconditions: PlanPreconditionObservation,
    lease: AuthorityLease,
    authorities: &DeterministicAuthorities<'_>,
) -> RestoreAdmission {
    SettledRestorePlan::apply(checkpoint, rescue, preconditions, lease, authorities)
}

fn apply_a_plan_identity(
    plan: CheckpointRestorePlanId,
    rescue: RestoreRescue,
    preconditions: PlanPreconditionObservation,
    lease: AuthorityLease,
    authorities: &DeterministicAuthorities<'_>,
) -> RestoreAdmission {
    SettledRestorePlan::apply(plan, rescue, preconditions, lease, authorities)
}

fn main() {
    let _ = apply_a_bare_checkpoint;
    let _ = apply_a_plan_identity;
}
