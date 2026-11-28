//! FIRST-RUN-MODEL-MATCH: a ChatGPT account whose models the public catalog
//! does not describe yet still composes, from its own listing's metadata; any
//! other account keeps the catalog-only refusal.

use std::path::Path;

use grapt_platform::{PlatformOwner, PublishedConfig};

use super::shared::*;

const PRIMARY: &str = r#"
  "store":{"authority":"open-ready"},
  "product":{"build":"build-a"},
  "catalog":{"source":"models.dev-chatgpt"},
  "provider":{
    "id":"openai","model":"gpt-6-luna","base_model":"gpt-6-luna","family":"gpt-6-luna",
    "route":"responses","account":"account-a",
    "credential_version":"credential-v1","policy_version":"df0-policy-v1""#;

/// The files a ChatGPT first run stages when the catalog lags the account:
/// the catalog describes only `gpt-5.6`, which the account hides, and the
/// account lists `gpt-6-luna`, which the catalog does not describe.
fn write_lagging_catalog(root: &Path, accounts: &str) -> TestResult {
    std::fs::write(
        root.join("grapt.jsonc"),
        format!("{{{PRIMARY}{accounts}}}\n}}"),
    )?;
    std::fs::write(
        root.join("provider.credential"),
        "sentinel-provider-secret\n",
    )?;
    std::fs::write(
        root.join("model-catalog.json"),
        r#"{
  "openai":{"id":"openai","name":"OpenAI","npm":"@ai-sdk/openai","models":{
    "gpt-5.6":{"id":"gpt-5.6","name":"GPT-5.6","family":"gpt-5"}
  }},
  "openrouter":{"id":"openrouter","name":"OpenRouter","npm":"@openrouter/ai-sdk-provider","models":{
    "vendor-model-a":{"id":"vendor-model-a","name":"Model A","family":"model-a"}
  }}
}"#,
    )?;
    std::fs::write(
        root.join("account-models.json"),
        r#"{"models":[
          {"slug":"gpt-5.6","visibility":"hide"},
          {"slug":"gpt-6-luna","display_name":"GPT-6-Luna","visibility":"list",
           "shell_type":"unified_exec","input_modalities":["text","image"],
           "context_window":272000,"supported_reasoning_levels":[{"effort":"low"}]}
        ]}"#,
    )?;
    Ok(())
}

#[test]
fn a_chatgpt_primary_composes_a_model_only_its_listing_describes() -> TestResult {
    let root = scratch("listing-metadata-primary")?;
    ready_database(&root)?;
    write_lagging_catalog(&root, "")?;
    drop(PublishedConfig::load(&root)?);
    let owner =
        PlatformOwner::prepare_with_authorities(&root, &FixedAuthorities, &FixedAuthorities)?;
    let (store, targets, _build, _credential, _first_contact) =
        owner.into_provider_parts()?.into_components();
    drop(store);
    let inventory = targets
        .inventory()
        .ok_or("a composed inventory is required")?;
    let models: Vec<_> = inventory
        .available()
        .iter()
        .map(|model| model.id().as_str())
        .collect();
    assert_eq!(models, vec!["gpt-6-luna"], "the hidden model is not issued");
    let luna = &inventory.available()[0];
    assert_eq!(luna.metadata()["metadata_source"], "account-listing");
    assert_eq!(luna.metadata()["name"], "GPT-6-Luna");
    assert_eq!(luna.context(), Some(258400));
    let target = targets.issue()?;
    assert_eq!(target.model_ref().model().as_str(), "gpt-6-luna");
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_secondary_account_still_refuses_a_model_its_catalog_does_not_describe() -> TestResult {
    let root = scratch("listing-metadata-secondary")?;
    ready_database(&root)?;
    write_lagging_catalog(
        &root,
        r#",
    "accounts":[
      {"id":"openrouter","route":"route-openrouter","account":"account-b",
       "credential_version":"credential-b1","policy_version":"df0-policy-v1",
       "protocol_family":"openai-compatible",
       "endpoint":"https://openrouter.ai/api/v1",
       "credential":"openrouter.credential",
       "discovery":"openrouter-account-models.json"}
    ]"#,
    )?;
    std::fs::write(
        root.join("openrouter.credential"),
        "sentinel-openrouter-secret\n",
    )?;
    std::fs::write(
        root.join("openrouter-account-models.json"),
        r#"{"models":[{"slug":"vendor-model-new","visibility":"list","context_window":128000,
            "supported_reasoning_levels":[]}]}"#,
    )?;
    let refusal = match PlatformOwner::prepare_with_authorities(
        &root,
        &FixedAuthorities,
        &FixedAuthorities,
    ) {
        Ok(_) => return Err("a secondary account issued an undescribed model".into()),
        Err(refusal) => refusal.to_string(),
    };
    assert!(
        refusal.contains("an account model has no catalog metadata")
            && !refusal.contains("vendor-model-new"),
        "{refusal}"
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn malformed_context_never_borrows_the_listing_maximum() -> TestResult {
    let root = scratch("listing-context-refusal")?;
    ready_database(&root)?;
    write_lagging_catalog(&root, "")?;
    for context in [
        serde_json::json!("unreadable"),
        serde_json::json!(-1),
        serde_json::json!(1.5),
        serde_json::json!(0),
        serde_json::json!(u64::MAX),
        serde_json::json!({}),
    ] {
        let listing = serde_json::json!({"models":[{"slug":"gpt-6-luna", "visibility":"list", "context_window":context,
            "max_context_window":1000,"supported_reasoning_levels":[]}]});
        std::fs::write(
            root.join("account-models.json"),
            serde_json::to_vec(&listing)?,
        )?;
        assert!(
            PublishedConfig::load(&root).is_err(),
            "malformed context {context} was published"
        );
    }
    for (context, expected) in [
        // Unstamped known models use their standard window, never the maximum.
        (Some(serde_json::Value::Null), 258400),
        (None, 258400),
        (Some(serde_json::json!(2000)), 258400),
    ] {
        let mut row = serde_json::json!({"slug":"gpt-6-luna", "visibility":"list", "max_context_window":1000,"supported_reasoning_levels":[]});
        if let Some(context) = context {
            row["context_window"] = context;
        }
        std::fs::write(
            root.join("account-models.json"),
            serde_json::to_vec(&serde_json::json!({"models":[row]}))?,
        )?;
        let published = PublishedConfig::load(&root)?;
        let inventory = published.inventory().ok_or("missing inventory")?;
        assert_eq!(inventory.available()[0].context(), Some(expected));
    }
    std::fs::remove_dir_all(root)?;
    Ok(())
}
