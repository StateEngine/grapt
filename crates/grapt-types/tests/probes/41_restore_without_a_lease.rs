// Probe 41 (row 12, second clause): a restore cannot be applied without the
// consuming authority lease.
//
// Amendment §8.3: "Every mutation or external effect crosses one governed entry
// carrying a non-clone authority lease through effect completion." Amendment
// §8.5: "A destructive provider operation needs explicit authority even if
// capture/inspect is policy-automatic. An agent cannot auto-confirm a heavy
// restore merely because it may capture or inspect checkpoints."
//
// `SettledRestorePlan::apply` takes the lease by value and there is no second
// entry, no `apply_unchecked`, and no defaulted lease. Holding the settled plan
// — which already means a planner enumerated the exact consequences — and even
// holding a live precondition observation does not authorize the write.
//
// Error phase: arity, resolved during type checking. Every other value the
// entry needs is supplied as a parameter, so the ONLY thing missing is the
// lease and the arity error is what the compiler reports. (An earlier revision
// of this probe referenced an undeclared `preconditions` binding, which made
// E0425 — an unrelated name-resolution error — the recorded first error and
// witnessed nothing about authority.)
use grapt_types::{
    DeterministicAuthorities, PlanPreconditionObservation, RestoreAdmission, RestoreRescue,
    SettledRestorePlan,
};

fn restore_without_authority(
    plan: SettledRestorePlan,
    rescue: RestoreRescue,
    preconditions: PlanPreconditionObservation,
    authorities: &DeterministicAuthorities<'_>,
) -> RestoreAdmission {
    plan.apply(rescue, preconditions, authorities)
}

fn main() {
    let _ = restore_without_authority;
}
