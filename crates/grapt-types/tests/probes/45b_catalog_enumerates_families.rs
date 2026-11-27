// Probe 45b: the same reconstruction, taken through the CATALOG instead of the
// ref.
//
// Split from 44 because it is a different route to the same law, and because
// sealing only `ModelRef::family()` would have left the wider version of the
// bypass wide open: a caller holding the runtime's loaded snapshot could
// enumerate every row and read the genuine family of every target the runtime
// carries — not one answer, the whole table. From there, "may `agent default`
// cross to this target" is a lookup, and no sealed predicate is ever called.
//
// A model picker that legitimately needs to LIST targets is a real requirement.
// It belongs to the model registry/config lane, and it must be served a
// projection carrying the endpoint coordinates only.
// The probe reads the family THROUGH the enumeration rather than stopping at
// it, so it witnesses the whole route: the seal on `records()` and the seal on
// the row's own accessor are two independent locks on one door, and a probe
// that stopped at the first would not notice if the second went missing.
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

    // The step that must not exist: reading the routing truth straight off the
    // authority that holds it.
    let _families: Vec<&ModelFamilyId> = runtime
        .records()
        .iter()
        .map(ModelCatalogRecord::family)
        .collect();
}
