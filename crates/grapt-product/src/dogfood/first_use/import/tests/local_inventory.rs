//! Setup imports the metadata the live local-provider configuration consumes.
use super::*;

#[test]
fn sc_fr01_setup_imports_a_local_listing_without_a_composed_catalog() -> Result<(), String> {
    for (with_catalog, with_secondary) in [(false, false), (true, false), (true, true)] {
        let scratch = Scratch::new("sc-fr01-import-listing")?;
        let source = scratch.account()?;
        let mut config = serde_json::json!({"provider":{
            "id":"vllm","model":"qwen3.8-27b","base_model":"qwen3.8-27b","family":"qwen3",
            "protocol_family":"openai-compatible","endpoint":"https://local.example/v1",
            "route":"test-route","account":"test-account","credential_version":"test-v1",
            "policy_version":"test-policy","variants":[]
        }});
        if with_secondary {
            config["provider"]["accounts"] = serde_json::json!([{
                "id":"openai","protocol_family":"openai-responses",
                "route":"secondary-route","account":"secondary-account",
                "credential_version":"secondary-v1","policy_version":"test-policy",
                "credential":"openai.credential","discovery":"openai-models.json"
            }]);
        }
        std::fs::write(
            source.join("grapt.jsonc"),
            serde_json::to_vec(&config).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let target = scratch.0.join("new-profile");
        let error = initialize(&target, &source)
            .err()
            .ok_or("missing local discovery accepted")?;
        assert!(error.contains("account-models.json"), "{error}");
        assert!(!target.exists());
        let listing = br#"{"data":[{"id":"qwen3.8-27b","max_model_len":200000}]}"#;
        std::fs::write(source.join("account-models.json"), listing)
            .map_err(|error| error.to_string())?;
        if with_catalog {
            let catalog = br#"{
                "vllm":{"id":"vllm","name":"Local Qwen","models":{}},
                "openai":{"id":"openai","models":{"secondary-model":{
                    "id":"secondary-model","family":"secondary-family","limit":{"context":16000}
                }}}
            }"#;
            std::fs::write(source.join("model-catalog.json"), catalog)
                .map_err(|error| error.to_string())?;
        }
        let secondary_listing = br#"{"models":[{"slug":"secondary-model","context_window":16000,"effective_context_window_percent":100,"supported_reasoning_levels":[]}]}"#;
        if with_secondary {
            std::fs::write(source.join("openai-models.json"), secondary_listing)
                .map_err(|error| error.to_string())?;
            std::fs::write(
                source.join("openai.credential"),
                b"SECONDARY_DUMMY_NO_PROVIDER_CALL",
            )
            .map_err(|error| error.to_string())?;
        }
        let receipt: serde_json::Value = serde_json::from_str(&initialize(&target, &source)?)
            .map_err(|error| error.to_string())?;
        assert_eq!(receipt["provider_calls"], 0);
        assert_eq!(
            capture(&target.join("account-models.json"), INVENTORY_BYTES)?,
            listing
        );
        assert_eq!(target.join("model-catalog.json").exists(), with_catalog);
        let published =
            grapt_platform::PublishedConfig::load(&target).map_err(|error| error.to_string())?;
        let inventory = published
            .inventory()
            .ok_or("local inventory missing after import")?;
        let model = inventory
            .available()
            .first()
            .ok_or("imported model missing")?;
        assert_eq!(model.context(), Some(200000));
        assert_eq!(model.metadata()["family"], "qwen3");
        if with_secondary {
            assert_eq!(
                capture(&target.join("openai-models.json"), INVENTORY_BYTES)?,
                secondary_listing
            );
            let secondary = inventory
                .available()
                .get(1)
                .ok_or("secondary model lost during setup")?;
            assert_eq!(secondary.provider().as_str(), "openai");
            assert_eq!(secondary.context(), Some(16000));
        }
    }
    Ok(())
}

#[test]
fn sc_fr01_setup_does_not_ignore_an_unreadable_optional_catalog() -> Result<(), String> {
    let scratch = Scratch::new("sc-fr01-import-invalid-catalog")?;
    let source = scratch.account()?;
    // Reuse the explicit profile's original fields; only the protocol changes.
    let original = String::from_utf8(capture(&source.join("grapt.jsonc"), CONFIG_BYTES)?)
        .map_err(|error| error.to_string())?;
    std::fs::write(
        source.join("grapt.jsonc"),
        original.replace(
            "\"id\":\"openai\"",
            "\"id\":\"openai\",\"protocol_family\":\"openai-responses\"",
        ),
    )
    .map_err(|error| error.to_string())?;
    std::fs::write(
        source.join("account-models.json"),
        br#"{"data":[{"id":"test-model","max_model_len":16000}]}"#,
    )
    .map_err(|error| error.to_string())?;
    std::fs::create_dir(source.join("model-catalog.json")).map_err(|error| error.to_string())?;
    let target = scratch.0.join("new-profile");
    let error = initialize(&target, &source)
        .err()
        .ok_or("nonregular optional catalog ignored")?;
    assert!(error.contains("regular file"), "{error}");
    assert!(!target.exists());
    Ok(())
}
