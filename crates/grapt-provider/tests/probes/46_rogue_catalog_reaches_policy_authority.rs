// Probe 46: a rogue catalog cannot enter POLICY authority.
//
// This is the external half of the chain seal — external because the law spans
// three crates and only a consumer crate can witness it end to end.
//
// The attacker's chain was: author two `ModelCatalogRecord`s, load them into a
// catalog of its own, take an `entry`, build a `ModelRef` from it, and hand that
// ref to `SnapshotBuilder::model`. Every step is public and stays public — a
// `ModelRef` is inert metadata and downstream crates legitimately hold one in a
// provider request. What changed is the last step: the builder now requires a
// registry-issued `ResolvedModelTarget`, whose mint is crate-private to
// `grapt-types` with no public route at all.
//
// Sealing `same_family` and `resolve` alone did NOT close this. That is the
// finding: the predicate was sealed while the door it guarded stayed open, so a
// rogue ref still became a real `PhysicalAttemptAuthoritySnapshot`.
use grapt_policy::SnapshotBuilder;
use grapt_types::{
    BaseModelId, ModelCatalog, ModelCatalogRecord, ModelCatalogVersion, ModelFamilyId, ModelId,
    ModelRef, ModelVariantId, ProviderId,
};

fn main() {
    let record = ModelCatalogRecord::new(
        ProviderId::new("anthropic").expect("provider"),
        ModelId::new("attacker-model").expect("model"),
        BaseModelId::new("attacker-model").expect("base"),
        ModelFamilyId::new("claude").expect("family"),
        None::<ModelVariantId>,
    );
    let catalog = ModelCatalog::load(
        ModelCatalogVersion::new("rogue-release").expect("version"),
        vec![record],
    )
    .expect("a self-consistent rogue release loads");

    let rogue = ModelRef::from_catalog(
        catalog
            .entry(
                &ProviderId::new("anthropic").expect("provider"),
                &ModelId::new("attacker-model").expect("model"),
                None,
            )
            .expect("row present"),
    );

    // The step that must not exist.
    let mut builder = SnapshotBuilder::new();
    let _ = builder.model(rogue);
}
