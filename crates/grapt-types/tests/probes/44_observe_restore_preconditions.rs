// Probe 44: a restore's preconditions cannot be self-observed.
//
// The plan binds provider version, negotiated lanes and worktree quiescence,
// and until this round nothing ever looked at them — `apply` checked expiry,
// capability and rescue only, so an application could enter with a stale
// provider, an unnegotiated lane or a dirty worktree, which are precisely the
// conditions the plan recorded in order to prevent it.
//
// `apply` now demands a `PlanPreconditionObservation`. Only something that can
// actually talk to the provider may claim one, and this crate cannot: it has no
// provider. So the mint is crate-private and there is no public route to a
// value the governed entry will accept.
use grapt_types::PlanPreconditionObservation;

fn main() {
    let _observation = PlanPreconditionObservation::observe(todo!(), todo!(), todo!());
}
