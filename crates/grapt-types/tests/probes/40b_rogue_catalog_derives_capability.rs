// Probe 40b: the same rogue snapshot, aimed at capability identity instead of
// the family boundary.
//
// `ResolvedModelTarget::resolve` DERIVES the `CapabilityId` a physical attempt
// runs under. Reachable from a catalog the caller authored, it mints capability
// identity for coordinates nobody vouched for. Split from probe 40 because it is
// a different law — one is the `agent default` boundary, the other is capability
// derivation — even though both now fail in the privacy phase.
use grapt_types::{
    AccountId, BaseModelId, CredentialVersion, ModelCatalog, ModelCatalogRecord,
    ModelCatalogVersion, ModelFamilyId, ModelId, ModelRef, ModelRouteId, ModelVariantId,
    PolicyVersion, ProviderId, ResolvedModelTarget,
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
    .expect("rogue release loads");

    let model = ModelRef::from_catalog(
        catalog
            .entry(
                &ProviderId::new("anthropic").expect("provider"),
                &ModelId::new("attacker-model").expect("model"),
                None,
            )
            .expect("row present"),
    );

    // The step that must not exist: capability identity from authored rows.
    let _target = ResolvedModelTarget::resolve(
        model,
        ModelRouteId::new("rt-invented").expect("route"),
        AccountId::new("acct-invented").expect("account"),
        CredentialVersion::new("1").expect("credential"),
        PolicyVersion::new("policy-invented").expect("policy"),
    );
}
