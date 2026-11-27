// Probe 2, second clause: a SessionGrant has no Display (hence no ToString) and
// no Clone, so it cannot be handed to a persistence or serialization boundary
// and cannot be duplicated out of the policy owner holding it.
//
// Nothing is constructed here on purpose: these must be trait-selection
// failures, not privacy failures.
use grapt_types::SessionGrant;

fn persist<T: ToString>(_value: T) {}

fn cannot_persist(grant: SessionGrant) {
    persist(grant);
}

fn cannot_duplicate(grant: &SessionGrant) -> SessionGrant {
    grant.clone()
}

fn main() {
    let _ = (cannot_persist, cannot_duplicate);
}
