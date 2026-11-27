// Probe 02 (R4H-L invariant 4): a bound listener has exactly one owner.
//
// Two owners of one socket cannot both be responsible for closing it, and the
// later API host takes the listener by value. `BoundListener` therefore does
// not implement `Clone`, and this asks the compiler to confirm that rather than
// asserting it about a value at runtime — where a `Clone` impl would already
// have done the damage before any assertion ran.
//
// This probe carries exactly one error phase: the bound is unsatisfied. It
// needs no listener value, and deliberately constructs none, so nothing here
// can fail for an incidental reason such as an unavailable port.

use grapt_location::BoundListener;

fn requires_clone<T: Clone>() {}

fn main() {
    requires_clone::<BoundListener>();
}
