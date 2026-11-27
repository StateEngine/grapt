// Probe 48b: a catalog row cannot be ordered or hashed either.
//
// This is the doubled-seal half of probe 48. `ModelCatalogRecord` derived
// `PartialOrd`, `Ord` and `Hash` alongside `PartialEq`, and each is the same
// oracle wearing a different hat: `Ord` compares `base_model` and `family`
// lexically, which answers MORE than equality does — it bisects the family
// space instead of testing one candidate at a time — and `Hash` restores exact
// equality through digest comparison. Removing `PartialEq` while leaving these
// would have closed nothing.
//
// `ModelCatalog::load` used to depend on the row `Ord` for its sort/dedup. It
// now sorts by lookup coordinate, which is all the self-contradiction check
// ever needed: rows claiming one coordinate become adjacent either way.
//
// Both bounds live in one file because both are the same error phase — trait
// selection — so rustc reports them together rather than aborting after the
// first.
fn requires_ord<T: Ord>() {}
fn requires_hash<T: core::hash::Hash>() {}

fn main() {
    requires_ord::<grapt_types::ModelCatalogRecord>();
    requires_hash::<grapt_types::ModelCatalogRecord>();
}
