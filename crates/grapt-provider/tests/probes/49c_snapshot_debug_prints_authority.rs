// Probe 49c: an authority snapshot cannot be printed.
//
// The snapshot's derived `Debug` reached its `ResolvedModelTarget`, and through
// it the derived `CapabilityId` whose canonical form is the literal
// `…|base=…|family=…|…`. A single tracing line at the request boundary
// therefore published both sealed catalog facts — the shortest route of the
// four leak channels the boundary seals, and the one most likely to be added
// back by a well-meaning diagnostic.
//
// It is absent rather than redacted so that a field added to the graph later
// cannot reopen it without someone writing the formatter by hand and deciding
// what the new field is. `SnapshotBuilder` keeps a hand-written `Debug` that
// reports contributor COUNTS and whether a model is set, which is what a
// duplicate-contributor or missing-model diagnostic actually needs.
fn requires_debug<T: core::fmt::Debug>() {}

fn main() {
    requires_debug::<grapt_policy::PhysicalAttemptAuthoritySnapshot>();
}
