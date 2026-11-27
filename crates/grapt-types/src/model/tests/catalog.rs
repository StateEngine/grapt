use super::{exact_row, load_exact, row, PROVIDER};
use crate::model::test_catalog::{catalog, record};
use crate::model::*;

#[test]
fn a_catalog_that_contradicts_itself_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let contradiction = vec![
        record("anthropic", "claude", "m").ok_or("record")?,
        record("anthropic", "cheap-tier", "m").ok_or("record")?,
    ];
    assert!(
        ModelCatalog::load(
            ModelCatalogVersion::new("cat-1").ok_or("version")?,
            contradiction
        )
        .is_none(),
        "a release cannot put one model in two families"
    );

    // An exactly repeated row asserts nothing new and is collapsed.
    let repeated = vec![
        record("anthropic", "claude", "m").ok_or("record")?,
        record("anthropic", "claude", "m").ok_or("record")?,
    ];
    let loaded = ModelCatalog::load(
        ModelCatalogVersion::new("cat-1").ok_or("version")?,
        repeated,
    )
    .ok_or("catalog")?;
    assert_eq!(loaded.records().len(), 1);
    Ok(())
}

#[test]
fn a_model_absent_from_the_catalog_cannot_be_named() -> Result<(), Box<dyn std::error::Error>> {
    let catalog = catalog("cat-1", &[("anthropic", "claude", "claude-opus-5")]).ok_or("catalog")?;
    assert!(
        catalog
            .entry(
                &ProviderId::new("anthropic").ok_or("provider")?,
                &ModelId::new("not-in-the-catalog").ok_or("model")?,
                None
            )
            .is_none(),
        "an uncatalogued target has no base model and no family"
    );
    // The variant is part of the lookup coordinate, not a hint.
    assert!(
        catalog
            .entry(
                &ProviderId::new("anthropic").ok_or("provider")?,
                &ModelId::new("claude-opus-5").ok_or("model")?,
                Some(&ModelVariantId::new("thinking").ok_or("variant")?)
            )
            .is_none(),
        "a variant the release never declared is not the declared row"
    );
    Ok(())
}

#[test]
fn byte_distinct_spellings_are_distinct_coordinates() -> Result<(), Box<dyn std::error::Error>> {
    // Model registry identities are exact UTF-8 byte identity: no implicit
    // trim, case fold, Unicode normalization or lossy rewrite. A future
    // normalization rule is a compatibility change and a separate lane.
    let catalog = load_exact(
        "cat-1",
        vec![row("claude-opus-5", "claude-opus-5", "claude", None).ok_or("row")?],
    )
    .ok_or("catalog")?;

    let lower = ProviderId::new(PROVIDER).ok_or("provider")?;
    let upper = ProviderId::new("Anthropic").ok_or("provider")?;
    let endpoint = ModelId::new("claude-opus-5").ok_or("model")?;

    assert!(
        catalog.entry(&upper, &endpoint, None).is_none(),
        "a case-different provider spelling is a DIFFERENT coordinate and must \
         not resolve the row this release actually carries"
    );

    // And the two spellings coexist in one release, because they never
    // collide: the contradiction check compares coordinates byte-wise.
    let both = load_exact(
        "cat-2",
        vec![
            row("claude-opus-5", "claude-opus-5", "claude", None).ok_or("lower row")?,
            exact_row(
                "Anthropic",
                "claude-opus-5",
                "claude-opus-5",
                "claude",
                None,
            )
            .ok_or("upper row")?,
        ],
    )
    .ok_or("byte-distinct spellings are not a self-contradicting release")?;
    assert!(both.entry(&lower, &endpoint, None).is_some());
    assert!(both.entry(&upper, &endpoint, None).is_some());
    Ok(())
}
