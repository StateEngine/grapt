// Probe 47b: a `ResolvedModelTarget` cannot be duplicated.
//
// This one is not a read channel, it is a REUSE channel. The resolved target is
// the model registry's statement that one physical attempt may run against one
// target; `SnapshotBuilder::model` consumes it by value so that one issuance
// admits one authority graph. `Clone` defeated that silently — a caller kept a
// copy back and admitted the same registry statement to a second graph, and
// "one immutable receipted authority graph per physical attempt" held only by
// convention.
//
// The bound is ASSERTED rather than `.clone()` called: `.clone()` on a value
// autorefs through the always-`Clone` reference type and would compile whatever
// this type implements, witnessing nothing.
fn requires_clone<T: Clone>() {}

fn main() {
    requires_clone::<grapt_types::ResolvedModelTarget>();
}
