// Probe 45c: the third route to the same coordinates — through the entry the
// runtime's catalog issues.
//
// `ModelCatalog::entry` is public, and it must be: naming a coordinate is how a
// legitimate caller asks for a target. What it hands back is a
// `ModelCatalogEntry`, and an entry is a token to be CONSUMED by
// `ModelRef::from_catalog`. If it also yields its row, then every caller that
// may ask "does the runtime carry this model" may also read what family the
// runtime puts it in — which is probe 45's exploit reached one call earlier.
//
// The accessor is DELETED rather than made crate-private: nothing in the crate
// calls it, and a private stub that exists only to be refused would be a
// misleading API preserved by suppression. So the error here is "no such
// method", not "private method", and that is the stronger statement.
use grapt_types::{
    BaseModelId, ModelCatalog, ModelCatalogRecord, ModelCatalogVersion, ModelFamilyId, ModelId,
    ModelVariantId, ProviderId,
};

fn main() {
    let runtime = ModelCatalog::load(
        ModelCatalogVersion::new("runtime-release").expect("version"),
        vec![ModelCatalogRecord::new(
            ProviderId::new("anthropic").expect("provider"),
            ModelId::new("expensive-model").expect("model"),
            BaseModelId::new("expensive-model").expect("base"),
            ModelFamilyId::new("premium").expect("family"),
            None::<ModelVariantId>,
        )],
    )
    .expect("the runtime release loads");

    let entry = runtime
        .entry(
            &ProviderId::new("anthropic").expect("provider"),
            &ModelId::new("expensive-model").expect("model"),
            None,
        )
        .expect("row present");

    // The step that must not exist: unwrapping the issued token back into the
    // catalog's own statement.
    let _row = entry.record();
}
