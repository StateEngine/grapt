// Probe 45: reconstructing the sealed `agent default` boundary BY HAND.
//
// Probe 40 sealed `ModelRef::same_family`. That was necessary and not
// sufficient, and this file is the reason. The predicate is
//
//     same snapshot && same provider && same family
//
// and a caller that can READ the coordinates off two refs simply writes the
// conjunction itself. Sealing the function while leaving its evidence public
// guards the door and hands out the key: `parent.family() == child.family()`
// IS `same_family`, minus the same-snapshot clause that makes it fail closed —
// so the hand-rolled version is not merely equivalent, it is strictly WORSE, and
// says `true` across two catalogs where the sealed one says `false`.
//
// The attack does not even need a rogue release. It is written here with the
// RUNTIME's own catalog, because reading the genuine family of a genuine target
// is the whole exploit: knowing it, the attacker authors a rogue row that
// matches, and every downstream comparison-by-string agrees.
//
// `provider()` and `model()` stay public deliberately — they answer "which
// endpoint do I call", which is not an authority question. `family()` and
// `base_model()` do not.
use grapt_types::{
    BaseModelId, ModelCatalog, ModelCatalogRecord, ModelCatalogVersion, ModelFamilyId, ModelId,
    ModelRef, ModelVariantId, ProviderId,
};

fn row(model: &str, family: &str) -> ModelCatalogRecord {
    ModelCatalogRecord::new(
        ProviderId::new("anthropic").expect("provider"),
        ModelId::new(model).expect("model"),
        BaseModelId::new(model).expect("base"),
        ModelFamilyId::new(family).expect("family"),
        None::<ModelVariantId>,
    )
}

fn target(catalog: &ModelCatalog, model: &str) -> ModelRef {
    ModelRef::from_catalog(
        catalog
            .entry(
                &ProviderId::new("anthropic").expect("provider"),
                &ModelId::new(model).expect("model"),
                None,
            )
            .expect("row present"),
    )
}

fn main() {
    let runtime = ModelCatalog::load(
        ModelCatalogVersion::new("runtime-release").expect("version"),
        vec![
            row("expensive-model", "premium"),
            row("cheap-model", "economy"),
        ],
    )
    .expect("the runtime release loads");

    let parent = target(&runtime, "expensive-model");
    let child = target(&runtime, "cheap-model");

    // The step that must not exist: the sealed decision, rewritten by hand out
    // of publicly readable evidence.
    let _may_cross = parent.provider() == child.provider() && parent.family() == child.family();
}
