// Probe 01: no bound type implements `Default`.
//
// A bound with a `Default` is a bound nobody decided. Every bound is built
// through `try_new`, which refuses zero and above-ceiling; a `Default` would let
// a call site skip that decision and inherit a number no ruling chose.
//
// Exactly one error phase: the only fault is that the item does not exist.

fn main() {
    let _ = grapt_config::MaxValueDepth::default();
}
