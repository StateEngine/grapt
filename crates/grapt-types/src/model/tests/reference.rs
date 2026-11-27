use crate::model::test_catalog::{catalog, model_ref};
use crate::model::*;

#[test]
fn model_ref_names_all_coordinates() -> Result<(), Box<dyn std::error::Error>> {
    let catalog =
        catalog("cat-1", &[("anthropic", "claude", "claude-fable-5")]).ok_or("catalog")?;
    let r = model_ref(&catalog, "anthropic", "claude-fable-5").ok_or("model")?;
    assert_eq!(r.provider().as_str(), "anthropic");
    assert_eq!(r.base_model().as_str(), "claude-fable-5");
    assert_eq!(r.family().as_str(), "claude");
    assert!(r.variant().is_none());
    assert_eq!(
        r.catalog_version().as_str(),
        "cat-1",
        "the ref records which release vouched for its family"
    );
    Ok(())
}

#[test]
fn family_membership_requires_the_same_provider() -> Result<(), Box<dyn std::error::Error>> {
    let catalog = catalog(
        "cat-1",
        &[
            ("anthropic", "claude", "m"),
            ("bedrock", "claude", "m"),
            ("anthropic", "other", "n"),
        ],
    )
    .ok_or("catalog")?;

    let anthropic = model_ref(&catalog, "anthropic", "m").ok_or("model")?;
    assert!(anthropic.same_family(&model_ref(&catalog, "anthropic", "m").ok_or("model")?));
    assert!(
        !anthropic.same_family(&model_ref(&catalog, "bedrock", "m").ok_or("model")?),
        "the same family name behind another provider is another target"
    );
    assert!(!anthropic.same_family(&model_ref(&catalog, "anthropic", "n").ok_or("model")?));
    Ok(())
}

#[test]
fn base_model_and_family_come_only_from_the_catalog() -> Result<(), Box<dyn std::error::Error>> {
    // The catalog says `claude-opus-5-20260801` projects to base
    // `claude-opus-5` in family `claude`. There is no public route that
    // takes a BaseModelId or a ModelFamilyId, so a caller cannot supply a
    // different projection and cannot trim a suffix off a marketing name.
    let record = ModelCatalogRecord::new(
        ProviderId::new("anthropic").ok_or("provider")?,
        ModelId::new("claude-opus-5-20260801").ok_or("model")?,
        BaseModelId::new("claude-opus-5").ok_or("base")?,
        ModelFamilyId::new("claude").ok_or("family")?,
        None,
    );
    let catalog = ModelCatalog::load(
        ModelCatalogVersion::new("cat-1").ok_or("version")?,
        vec![record],
    )
    .ok_or("catalog")?;
    let r = model_ref(&catalog, "anthropic", "claude-opus-5-20260801").ok_or("model")?;
    assert_eq!(r.base_model().as_str(), "claude-opus-5");
    assert_eq!(r.family().as_str(), "claude");
    Ok(())
}

#[test]
fn a_second_catalog_cannot_vouch_for_the_first_catalogs_targets(
) -> Result<(), Box<dyn std::error::Error>> {
    // The runtime's catalog: opus-5 is `claude`, and toy-1 is its own
    // unrelated `toy` family.
    let runtime = catalog(
        "cat-1",
        &[
            ("anthropic", "claude", "claude-opus-5"),
            ("anthropic", "toy", "toy-1"),
        ],
    )
    .ok_or("catalog")?;
    // The attack, written the only way that could ever have worked: the
    // rogue release copies the parent's REAL family name onto its own
    // model, so provider and family both match by string. It also copies
    // the runtime's version string to look legitimate.
    let rogue = catalog(
        "cat-1",
        &[
            ("anthropic", "claude", "claude-opus-5"),
            ("anthropic", "claude", "toy-1"),
        ],
    )
    .ok_or("catalog")?;

    let parent = model_ref(&runtime, "anthropic", "claude-opus-5").ok_or("model")?;
    let smuggled = model_ref(&rogue, "anthropic", "toy-1").ok_or("model")?;
    assert_eq!(
        parent.provider(),
        smuggled.provider(),
        "provider matches by string"
    );
    assert_eq!(
        parent.family(),
        smuggled.family(),
        "and so does the family name — this is what the rogue release bought"
    );
    assert!(
        !parent.same_family(&smuggled),
        "a family claim from another catalog is refused, not believed"
    );
    // The runtime's own answer about the same model is the honest one.
    assert!(!parent.same_family(&model_ref(&runtime, "anthropic", "toy-1").ok_or("model")?));

    // Even the rogue's byte-identical copy of the parent row cannot vouch
    // for the parent: identity is the snapshot allocation, not the text.
    let impostor_parent = model_ref(&rogue, "anthropic", "claude-opus-5").ok_or("model")?;
    assert_eq!(
        parent.catalog_version(),
        impostor_parent.catalog_version(),
        "the version strings are identical by construction"
    );
    assert_eq!(
        parent, impostor_parent,
        "and so are the durable coordinates"
    );
    assert!(
        !parent.same_family(&impostor_parent),
        "copying the version string does not copy the authority"
    );
    Ok(())
}

#[test]
fn equality_is_durable_coordinates_not_catalog_provenance() -> Result<(), Box<dyn std::error::Error>>
{
    let first = catalog("cat-1", &[("anthropic", "claude", "claude-opus-5")]).ok_or("catalog")?;
    let reloaded =
        catalog("cat-2", &[("anthropic", "claude", "claude-opus-5")]).ok_or("catalog")?;
    let a = model_ref(&first, "anthropic", "claude-opus-5").ok_or("model")?;
    let b = model_ref(&reloaded, "anthropic", "claude-opus-5").ok_or("model")?;
    assert_eq!(a, b, "two releases that agree name the same target");
    assert!(
        !a.same_family(&b),
        "agreement about a name is not shared family authority"
    );
    Ok(())
}

#[test]
fn one_issuance_is_stricter_than_equal_coordinates() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = catalog(
        "cat-1",
        &[
            ("anthropic", "claude", "claude-opus-5"),
            ("anthropic", "claude", "claude-haiku-4-5"),
        ],
    )
    .ok_or("catalog")?;
    let reloaded =
        catalog("cat-1", &[("anthropic", "claude", "claude-opus-5")]).ok_or("catalog")?;

    let opus = model_ref(&runtime, "anthropic", "claude-opus-5").ok_or("model")?;
    assert!(
        opus.is_same_issuance(&opus.clone()),
        "a clone is the same issuance — it carries the same snapshot handle"
    );
    assert!(
        opus.is_same_issuance(&model_ref(&runtime, "anthropic", "claude-opus-5").ok_or("model")?),
        "two lookups of one row in one snapshot are one issuance"
    );

    let impostor = model_ref(&reloaded, "anthropic", "claude-opus-5").ok_or("model")?;
    assert_eq!(opus, impostor, "the durable coordinates are equal");
    assert!(
        !opus.is_same_issuance(&impostor),
        "equal coordinates from another snapshot are NOT one issuance"
    );

    // The reason this predicate is safe to expose: it answers `false` for
    // exactly the pairs the family question is about — two DIFFERENT rows
    // of one snapshot — so it can never stand in for `same_family`.
    let sibling = model_ref(&runtime, "anthropic", "claude-haiku-4-5").ok_or("model")?;
    assert!(
        opus.same_family(&sibling),
        "same snapshot, provider, family"
    );
    assert!(
        !opus.is_same_issuance(&sibling),
        "yet issuance refuses them, so it leaks no family answer"
    );
    Ok(())
}
