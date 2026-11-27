// Probe 40: the COMPLETE rogue-snapshot chain to a cross-family routing answer.
//
// Probe 23 proved `ModelRef::new` is gone and probe 24 proved
// `ModelCatalogEntry` cannot be forged. Both are true and both miss this: the
// attacker does not need to forge an entry, because it can author BOTH ROWS and
// load them into one catalog of its own. Same-snapshot `Arc` identity stops a
// rogue catalog vouching for the runtime's targets — that part stands — but it
// is identity OF a snapshot, not authority OVER it. Here the attacker owns the
// whole snapshot, so both refs legitimately share it.
//
// The catalog types stay public deliberately: a `ModelRef` is inert coordinates
// that downstream crates hold in a provider request, and that is not authority.
// What is sealed is the authority-producing question — may `agent default` cross
// to this target — so the chain compiles right up to the moment it asks.
use grapt_types::{
    BaseModelId, ModelCatalog, ModelCatalogRecord, ModelCatalogVersion, ModelFamilyId, ModelId,
    ModelRef, ModelVariantId, ProviderId,
};

fn row(model: &str) -> ModelCatalogRecord {
    ModelCatalogRecord::new(
        ProviderId::new("anthropic").expect("provider"),
        ModelId::new(model).expect("model"),
        BaseModelId::new(model).expect("base"),
        // Both rows claim the same family. The attacker authored both.
        ModelFamilyId::new("claude").expect("family"),
        None::<ModelVariantId>,
    )
}

fn main() {
    let catalog = ModelCatalog::load(
        ModelCatalogVersion::new("rogue-release").expect("version"),
        vec![row("expensive-model"), row("attacker-model")],
    )
    .expect("a self-consistent rogue release loads");

    let parent = ModelRef::from_catalog(
        catalog
            .entry(
                &ProviderId::new("anthropic").expect("provider"),
                &ModelId::new("expensive-model").expect("model"),
                None,
            )
            .expect("row present"),
    );
    let child = ModelRef::from_catalog(
        catalog
            .entry(
                &ProviderId::new("anthropic").expect("provider"),
                &ModelId::new("attacker-model").expect("model"),
                None,
            )
            .expect("row present"),
    );

    // The step that must not exist: an authority answer from authored rows.
    let _may_cross = parent.same_family(&child);
}
