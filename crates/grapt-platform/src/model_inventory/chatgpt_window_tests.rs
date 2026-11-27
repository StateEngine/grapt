use super::*;
use serde_json::json;

#[test]
fn chatgpt_uses_the_standard_window_instead_of_a_stale_extended_listing() -> Result<(), String> {
    let catalog = json!({"openai":{"id":"openai","models":{
        "gpt-6.1-sol":{"id":"gpt-6.1-sol","family":"gpt","limit":{"context":1050000}}
    }}});
    let listing = json!({"models":[{"slug":"gpt-6.1-sol","visibility":"list",
        "context_window":500000,"max_context_window":872000,"supported_reasoning_levels":[]}]});
    let inventory = ModelInventory::chatgpt_listing(catalog.clone(), listing.clone())?;
    assert_eq!(inventory.available()[0].context(), Some(258400));
    let provider = ProviderId::new("openai").ok_or("provider")?;
    for (requested, expected) in [(200000, 190000), (872000, 828400), (2000000, 828400)] {
        let inventory = ModelInventory::compose_with_window(
            catalog.clone(),
            vec![(
                provider.clone(),
                listing.clone(),
                Metadata::CatalogOrListing,
                DiscoveryShape::Codex,
            )],
            Some(requested),
        )?;
        assert_eq!(inventory.available()[0].context(), Some(expected));
    }
    let now = grapt_types::ClockAuthority::now(&grapt_store::SystemClock).as_millis();
    for (age, expected) in [(0, 475000), (300001, 258400), (-86400000, 258400)] {
        let mut live = listing.clone();
        live["secode_fetched_at_ms"] = (now - age).into();
        let inventory = ModelInventory::chatgpt_listing(catalog.clone(), live)?;
        assert_eq!(inventory.available()[0].context(), Some(expected));
    }
    let unknown = json!({"models":[{"slug":"gpt-future","context_window":null,
        "max_context_window":400000,"supported_reasoning_levels":[]}]});
    assert!(ModelInventory::chatgpt_listing(catalog.clone(), unknown.clone()).is_err());
    let extended = ModelInventory::compose_with_window(
        catalog.clone(),
        vec![(
            provider,
            unknown,
            Metadata::CatalogOrListing,
            DiscoveryShape::Codex,
        )],
        Some(200000),
    )?;
    assert_eq!(extended.available()[0].context(), Some(190000));
    let api = ModelInventory::first_run(
        catalog,
        json!({"data":[{
            "id":"gpt-6.1-sol","max_model_len":500000
        }]}),
        "openai",
        "models.dev-openai",
        "openai-responses",
    )?;
    assert_eq!(api.available()[0].context(), Some(500000));
    Ok(())
}
