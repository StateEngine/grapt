use super::*;
use serde_json::json;

fn inventory(models: Value, rows: Value) -> Result<ModelInventory, String> {
    ModelInventory::first_run(
        json!({"openai":{"id":"openai","models":models}}),
        json!({"data":rows}),
        "openai",
        "models.dev-openai",
        "openai-responses",
    )
}

#[test]
fn sc_fr01_live_context_admits_a_model_without_public_catalog_metadata() -> Result<(), String> {
    let inventory = inventory(
        json!({}),
        json!([{"id":"qwen3.8-27b","max_model_len":200000}]),
    )?;
    let row = inventory
        .available()
        .first()
        .ok_or("listed model missing")?;
    assert_eq!(row.id().as_str(), "qwen3.8-27b");
    assert_eq!(row.provider().as_str(), "openai");
    assert_eq!(row.context(), Some(200000));
    assert_eq!(row.metadata()["metadata_source"], "provider-listing");
    assert!(row.metadata()["tool_call"].is_null());
    assert!(row.metadata()["modalities"]["input"].is_null());
    assert!(row.metadata()["limit"]["output"].is_null());
    assert!(!row.supports_images());
    Ok(())
}

#[test]
fn sc_fr01_unknown_context_remains_unknown_instead_of_zero_or_a_guessed_window(
) -> Result<(), String> {
    let inventory = inventory(json!({}), json!([{"id":"unannounced-model"}]))?;
    let row = inventory
        .available()
        .first()
        .ok_or("listed model missing")?;
    assert_eq!(row.context(), None);
    assert!(row.metadata()["limit"]["context"].is_null());
    assert_eq!(inventory.records()?.len(), 1);
    Ok(())
}

#[test]
fn sc_fr01_public_image_models_without_context_do_not_hide_other_listed_models(
) -> Result<(), String> {
    let inventory = inventory(
        json!({"image-model":{"id":"image-model","limit":{"context":0,"output":0}}}),
        json!([{"id":"image-model"},{"id":"chat-model","max_model_len":16000}]),
    )?;
    assert_eq!(inventory.available().len(), 2);
    assert_eq!(inventory.available()[0].context(), None);
    assert_eq!(inventory.available()[1].context(), Some(16000));
    Ok(())
}

#[test]
fn sc_fr01_provider_context_cannot_expand_the_public_context_ceiling() -> Result<(), String> {
    let models = json!({"model-a":{"id":"model-a","limit":{"context":32000,"output":1024}}});
    for (advertised, expected) in [(16000, 16000), (64000, 32000)] {
        let inventory = inventory(
            models.clone(),
            json!([{"id":"model-a","max_model_len":advertised}]),
        )?;
        assert_eq!(inventory.available()[0].context(), Some(expected));
        assert_eq!(inventory.available()[0].metadata()["limit"]["output"], 1024);
    }
    Ok(())
}

#[test]
fn sc_fr01_unreadable_limits_are_refused_without_becoming_unknown() {
    for invalid in [
        json!(0),
        json!(-1),
        json!(1.5),
        json!("200000"),
        json!(true),
    ] {
        assert!(inventory(json!({}), json!([{"id":"model-a","max_model_len":invalid}])).is_err());
    }
    assert!(inventory(json!({}), json!([{"id":"model-a"},{"id":"model-a"}])).is_err());
    assert!(inventory(json!({"model-a":{"id":"other"}}), json!([{"id":"model-a"}])).is_err());
}

#[test]
fn sc_fr01_endpoint_reported_fields_fill_metadata_without_inventing_absent_ones(
) -> Result<(), String> {
    let listed = inventory(
        json!({}),
        json!([{"id":"vendor/model","max_model_len":500000,
            "context_length":131072,"max_completion_tokens":16384,
            "supported_parameters":["tools","temperature"],
            "input_modalities":["text","image"],"output_modalities":["text"]}]),
    )?;
    let row = listed.available().first().ok_or("listed model missing")?;
    // The tighter of the explicit window and the endpoint's reported context.
    assert_eq!(row.context(), Some(131072));
    assert_eq!(row.metadata()["limit"]["context"], 131072);
    assert_eq!(row.metadata()["limit"]["output"], 16384);
    assert_eq!(row.metadata()["tool_call"], true);
    assert_eq!(
        row.metadata()["modalities"]["input"],
        json!(["text", "image"])
    );
    assert_eq!(row.metadata()["modalities"]["output"], json!(["text"]));
    assert!(row.supports_images());

    let bare = inventory(json!({}), json!([{"id":"vendor/model"}]))?;
    let row = bare.available().first().ok_or("listed model missing")?;
    assert_eq!(row.context(), None);
    assert!(row.metadata()["tool_call"].is_null());
    assert!(row.metadata()["modalities"]["input"].is_null());
    assert!(row.metadata()["limit"]["output"].is_null());

    // A reported context length never widens the explicit configured window.
    let wider = inventory(
        json!({}),
        json!([{"id":"vendor/model","max_model_len":100000,"context_length":900000}]),
    )?;
    assert_eq!(wider.available()[0].context(), Some(100000));
    Ok(())
}

#[test]
fn sc_fr01_listed_models_never_borrow_another_providers_metadata() -> Result<(), String> {
    let inventory = ModelInventory::first_run(
        json!({
            "openai":{"id":"openai","models":{}},
            "other":{"id":"other","models":{"model-a":{"id":"model-a","limit":{"context":900000}}}}
        }),
        json!({"data":[{"id":"model-a","max_model_len":16000}]}),
        "openai",
        "models.dev-openai",
        "openai-responses",
    )?;
    assert_eq!(inventory.connected().len(), 1);
    assert_eq!(inventory.available()[0].provider().as_str(), "openai");
    assert_eq!(inventory.available()[0].context(), Some(16000));
    assert_eq!(
        inventory.available()[0].metadata()["metadata_source"],
        "provider-listing"
    );
    Ok(())
}

#[test]
fn review_existing_provider_listing_accepts_an_unrelated_null_field() -> Result<(), String> {
    let listed = inventory(
        json!({}),
        json!([{"id":"model-a","max_model_len":16000,"supported_parameters":null}]),
    )?;
    assert_eq!(listed.available()[0].context(), Some(16000));
    assert!(listed.available()[0].metadata()["tool_call"].is_null());
    Ok(())
}

#[test]
fn review_endpoint_input_modalities_preserve_existing_output_modalities() -> Result<(), String> {
    let listed = inventory(
        json!({"model-a":{"id":"model-a","limit":{"context":32000,"output":1024},
            "modalities":{"input":["text"],"output":["text"]}}}),
        json!([{"id":"model-a","input_modalities":["text","image"]}]),
    )?;
    assert_eq!(
        listed.available()[0].metadata()["modalities"]["output"],
        json!(["text"])
    );
    Ok(())
}

#[test]
fn review_invalid_catalog_limit_remains_a_named_refusal() {
    let result = inventory(
        json!({"model-a":{"id":"model-a","limit":"unreadable"}}),
        json!([{"id":"model-a","max_completion_tokens":1024}]),
    );
    assert!(matches!(result, Err(error) if error == "model limits is not an object"));
}

#[test]
fn review_a_malformed_remote_context_is_unknown_not_a_refusal() -> Result<(), String> {
    // Drives the same function provider_listing uses for that field: a remote
    // report it cannot read must leave the field unknown, never refuse a
    // listing that was valid before enrichment.
    for broken in [json!("131072"), json!(-5), json!(0)] {
        let listed = inventory(
            json!({}),
            json!([{"id":"model-a","max_model_len":16000,"context_length":broken}]),
        )?;
        assert_eq!(
            listed.available()[0].context(),
            Some(16000),
            "a remote {broken} changed the admitted window"
        );
    }
    Ok(())
}

#[test]
fn a_listing_refusal_never_repeats_a_model_identity_the_provider_returned() -> Result<(), String> {
    for rows in [
        json!([{"id":"provider-chosen-name"},{"id":"provider-chosen-name"}]),
        json!([{"id":"provider-chosen-name","max_model_len":0}]),
    ] {
        let Err(refusal) = inventory(json!({}), rows) else {
            return Err("an invalid listing was admitted".into());
        };
        assert!(!refusal.contains("provider-chosen-name"), "{refusal}");
    }
    Ok(())
}
