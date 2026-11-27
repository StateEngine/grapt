use super::*;
use serde_json::json;

#[test]
fn account_count_is_refused_before_opening_any_snapshot() -> Result<(), String> {
    let account = InventoryAccount {
        provider: ProviderId::new("openai").ok_or("provider")?,
        catalog_provider: ProviderId::new("openai").ok_or("catalog provider")?,
        label: None,
        discovery: "account-models.json".into(),
        metadata: Metadata::Catalog,
        shape: DiscoveryShape::Codex,
    };
    // A NUL path cannot be opened on either supported platform. A filesystem
    // error here means the count guard ran too late.
    let root = Path::new("\0");
    for (accounts, expected) in [
        (
            Vec::new(),
            "an inventory needs at least one configured account",
        ),
        (
            vec![account; MAX_ACCOUNTS + 1],
            "configured account count exceeds its bound",
        ),
    ] {
        let error = ModelInventory::load(root, &accounts, None)
            .err()
            .ok_or("admitted")?;
        assert_eq!(error, expected);
    }
    Ok(())
}

fn model() -> Result<ConnectedModel, String> {
    Ok(ConnectedModel {
        provider: ProviderId::new("openai").ok_or("provider")?,
        id: ModelId::new("first").ok_or("model")?,
        metadata: json!({"id":"first","family":"first"}),
        context: Some(1000),
        variants: Vec::new(),
        default_variant: None,
        responses_lite: false,
        summary_parameter: false,
        effort_updates: false,
        thinking: None,
        original_image_detail: false,
        image_input: false,
    })
}

#[test]
fn variant_expansion_counts_against_the_record_bound() -> Result<(), String> {
    let mut inventory = ModelInventory {
        providers: BTreeMap::new(),
        connected: Vec::new(),
        available: vec![model()?; MAX_ISSUABLE_RECORDS / 2],
    };
    let low = ModelVariantId::new("low").ok_or("variant")?;
    for model in &mut inventory.available {
        model.variants.push(low.clone());
    }
    assert_eq!(inventory.records()?.len(), MAX_ISSUABLE_RECORDS);
    let first = inventory.available.first_mut().ok_or("first")?;
    first
        .variants
        .push(ModelVariantId::new("high").ok_or("variant")?);
    assert_eq!(
        inventory.records().err().ok_or("admitted excess variant")?,
        "configured accounts exceed the issuable record bound"
    );
    Ok(())
}

#[test]
fn model_admission_refuses_before_retaining_an_excess_row() -> Result<(), String> {
    let model = model()?;
    let mut available = vec![model.clone(); MAX_ISSUABLE_RECORDS - 1];
    let metadata = serde_json::Map::from_iter([("first".into(), model.metadata.clone())]);
    let account = json!({"models":[{
        "slug":"first", "context_window":1000, "supported_reasoning_levels":[]
    }]});
    ModelInventory::admit_account(
        &model.provider,
        &metadata,
        &account,
        Metadata::Catalog,
        &mut available,
    )?;
    assert_eq!(available.len(), MAX_ISSUABLE_RECORDS);
    assert_eq!(
        ModelInventory::admit_account(
            &model.provider,
            &metadata,
            &account,
            Metadata::Catalog,
            &mut available
        ),
        Err("configured accounts exceed the issuable model bound".into())
    );
    assert_eq!(available.len(), MAX_ISSUABLE_RECORDS);
    Ok(())
}
