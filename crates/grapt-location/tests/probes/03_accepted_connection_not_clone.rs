// Probe 03 (R4H-L accept seam): an admitted connection has exactly one owner.
//
// The accept seam exists so a host lane can serve connections without ever
// obtaining a second owner of the listening socket. That guarantee is worth
// nothing if the CONNECTION it hands back can be duplicated: two owners of one
// stream cannot both be responsible for closing it, and the consumer that
// serves the bytes must be the only one holding them.
//
// `AcceptedConnection` therefore does not implement `Clone`, and this asks the
// compiler to confirm it rather than asserting it about a value at runtime,
// where a `Clone` impl would already have done the damage before any assertion
// ran.
//
// This probe carries exactly one error phase: the bound is unsatisfied. It needs
// no connection value, and deliberately constructs none, so nothing here can
// fail for an incidental reason such as an unavailable port or a peer that never
// connects.

use grapt_location::AcceptedConnection;

fn requires_clone<T: Clone>() {}

fn main() {
    requires_clone::<AcceptedConnection>();
}
