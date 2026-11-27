// Probe 41b (row 12, second clause): one settlement applies at most once.
//
// The restore plan binds exact writes/deletes/bytes against an exact checkpoint
// and root. Applying it a second time is a second external effect that no
// authority settled — the consequences the answerer agreed to were counted for
// one application.
//
// `SettledRestorePlan` is not `Clone` and `apply` takes it by value, so the
// second call has nothing left to consume. Two rescues and two leases are
// supplied here on purpose: the caller has every *other* input it needs, and the
// plan alone stops it.
//
// Error phase: borrow checking (moved value). rustc reaches this only when the
// file is otherwise well typed, so it must be the only law in the file.
use grapt_types::{
    AuthorityLease, DeterministicAuthorities, PlanPreconditionObservation, RestoreAdmission,
    RestoreRescue, SettledRestorePlan,
};

fn apply_twice(
    plan: SettledRestorePlan,
    first_rescue: RestoreRescue,
    second_rescue: RestoreRescue,
    first_observation: PlanPreconditionObservation,
    second_observation: PlanPreconditionObservation,
    first_lease: AuthorityLease,
    second_lease: AuthorityLease,
    authorities: &DeterministicAuthorities<'_>,
) -> (RestoreAdmission, RestoreAdmission) {
    let first = plan.apply(first_rescue, first_observation, first_lease, authorities);
    let second = plan.apply(second_rescue, second_observation, second_lease, authorities);
    (first, second)
}

fn main() {
    let _ = apply_twice;
}
