// Probe 49b: two authority snapshots cannot be compared.
//
// Derived `PartialEq`/`Eq` on the snapshot compared its `ResolvedModelTarget`,
// inheriting that type's oracle over the catalog's `base_model` and `family`
// wholesale — a caller that could build or obtain snapshots read the sealed
// routing facts by comparing them, one candidate at a time.
//
// Comparing two snapshots is not a question this boundary needs. Comparing
// their receipts is, and `EffectiveGraphReceipt` still derives equality: after
// probe 59 it carries only the whole identity and digest, which is the exact
// surface §8.4 permits once no unkeyed secret-derived bytes enter the preimage.
fn requires_equality<T: PartialEq>() {}

fn main() {
    requires_equality::<grapt_policy::PhysicalAttemptAuthoritySnapshot>();
}
