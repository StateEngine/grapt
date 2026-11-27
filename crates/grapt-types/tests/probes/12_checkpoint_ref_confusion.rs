// Probe 12: a Git commit is not a workspace checkpoint, and neither is an
// artifact, a compaction, a Session fork or a restore plan.
//
// Row 12: "Passing a Git commit, artifact, compaction or Session-fork identity
// where a `WorkspaceCheckpointRef` is required, or applying a restore without
// the exact settled plan/authority lease."
//
// This file used to declare its own `fn restore(_: WorkspaceCheckpointRef)` and
// `fn apply_plan(_: CheckpointRestorePlanId)` stubs and confuse THOSE. It
// witnessed nothing about Grapt, because there was no restore surface in the
// crate to witness. Every call below now goes at the real governed boundary:
// `SettledRestorePlan::restores`, which asks whether a settled plan applies an
// exact checkpoint, and `SettledRestorePlan::apply`, the one entry that starts a
// destructive restore.
//
// The second clause of row 12 — applying without the exact settled plan or the
// consuming lease — is witnessed in probes 41, 41b, 42, 43, 43b and 43c, each in
// its own error phase.
use grapt_types::{
    ArtifactId, AuthorityLease, CheckpointHeadId, CheckpointRestorePlanId, CompactionId,
    DeterministicAuthorities, GitCommitId, RestoreAdmission, RestoreRescue, SessionId,
    SettledRestorePlan,
};

fn ask_the_real_boundary(plan: &SettledRestorePlan) {
    if let Some(commit) = GitCommitId::new("0123456789abcdef0123456789abcdef01234567") {
        plan.restores(&commit);
    }
    if let Some(artifact) = ArtifactId::new("art-1") {
        plan.restores(&artifact);
    }
    if let Some(compaction) = CompactionId::new("cmp-1") {
        plan.restores(&compaction);
    }
    if let Ok(fork) = "ses_fork".parse::<SessionId>() {
        plan.restores(&fork);
    }
    // A restore plan is not a checkpoint, and a checkpoint is not a plan.
    if let Some(restore_plan) = CheckpointRestorePlanId::new("plan-1") {
        plan.restores(&restore_plan);
    }
}

// A movable provider head is not the immutable settled plan the entry consumes.
fn apply_a_head_as_a_plan(
    head: CheckpointHeadId,
    rescue: RestoreRescue,
    lease: AuthorityLease,
    authorities: &DeterministicAuthorities<'_>,
) -> RestoreAdmission {
    SettledRestorePlan::apply(head, rescue, lease, authorities)
}

fn main() {
    let _ = ask_the_real_boundary;
    let _ = apply_a_head_as_a_plan;
}
