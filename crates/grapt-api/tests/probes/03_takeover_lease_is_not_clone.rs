// Probe 03 (R4H-A): a recipient lease is single-owner.
//
// The whole displacement law is that exactly one live lease represents a
// recipient. A `Clone` would produce a second holder of a single-owner claim,
// and both copies would answer `is_current` identically until one of them
// released the slot out from under the other.
//
// Asserted through a `T: Clone` BOUND rather than by calling `.clone()`:
// autoref makes `value.clone()` resolve on `&T` and compile even when `T` is
// not `Clone`, so a `.clone()` call is a witness that passes for the wrong
// reason.
use grapt_api::RecipientLease;

fn requires_clone<T: Clone>() {}

fn main() {
    requires_clone::<RecipientLease>();
}
