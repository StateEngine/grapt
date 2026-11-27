// Probe 8, second clause: one authority lease cannot authorize two effects.
// `AuthorityLease::consume` takes `self`, so the second call is a use after
// move.
//
// ATTRIBUTION REPAIR. This probe used to build its own lease with
// `AuthorityLease::new(derived, GrantScope::Once)`. Once that constructor was
// removed the file still failed — but its only error was `E0599: no associated
// function named new`, raised at the construction line BEFORE either
// `consume()`. So it had silently stopped witnessing linear non-reuse and had
// become a duplicate of probe 19, which already owns "a lease cannot be minted
// without a grant that covers it". A probe that fails for a reason unrelated to
// its law is worse than no probe, because the suite still shows green.
//
// The lease now arrives as a function ARGUMENT. That is deliberate: it removes
// every construction step from the file, so nothing can fail earlier than the
// move, and the recorded error is `E0382: use of moved value` — the law itself.
// Nothing here needs a lease to exist at runtime; the borrow checker decides it.
use grapt_types::{AuthorityLease, CapabilityId};

fn run_two_effects_on_one_authorization(lease: AuthorityLease) -> (CapabilityId, CapabilityId) {
    let first = lease.consume();
    let second = lease.consume();
    (first, second)
}

fn main() {
    let _ = run_two_effects_on_one_authorization;
}
