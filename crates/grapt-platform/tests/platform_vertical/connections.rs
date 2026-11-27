//! Connection IDs route attempts; catalog IDs supply model metadata.
use super::shared::*;
use grapt_platform::PublishedConfig;
use grapt_types::ProviderId;
use serde_json::json;
use std::path::PathBuf;

fn profile(label: &str) -> TestResult<PathBuf> {
    let root = scratch(label)?;
    std::fs::write(root.join("grapt.jsonc"), json!({
        "store":{"authority":"open-ready"}, "product":{"build":"connections"},
        "catalog":{"source":"models.dev-chatgpt"},
        "provider":{
            "id":"chatgpt", "catalog_provider":"openai", "label":"ChatGPT",
            "model":"gpt-6.1-sol", "base_model":"gpt-6.1-sol", "family":"gpt-6",
            "route":"chatgpt-route", "account":"subscription-account",
            "credential_version":"subscription-v1", "policy_version":"policy-v1", "variants":[],
            "accounts":[
                {"id":"openai", "catalog_provider":"openai", "label":"API key",
                 "route":"api-route", "account":"key-account", "credential_version":"key-v1",
                 "policy_version":"policy-v1", "protocol_family":"openai-responses",
                 "credential":"openai-key.credential", "discovery":"openai-key-models.json"},
                {"id":"claude-work", "catalog_provider":"anthropic", "label":"Work Claude",
                 "route":"messages-route", "account":"work-account", "credential_version":"work-v1",
                 "policy_version":"policy-v1", "protocol_family":"anthropic-messages",
                 "credential":"claude-work.credential", "discovery":"claude-work-models.json"}
            ]
        }
    }).to_string())?;
    std::fs::write(
        root.join("model-catalog.json"),
        json!({
            "openai":{"id":"openai", "name":"OpenAI", "models":{
                "gpt-6.1-sol":{"id":"gpt-6.1-sol", "name":"GPT", "limit":{"context":128000}}
            }},
            "anthropic":{"id":"anthropic", "name":"Anthropic", "models":{
                "claude-opus-5":{"id":"claude-opus-5", "name":"Claude", "limit":{"context":200000}}
            }}
        })
        .to_string(),
    )?;
    std::fs::write(
        root.join("account-models.json"),
        json!({"models":[{
            "slug":"gpt-6.1-sol", "context_window":272000,
            "supported_reasoning_levels":[{"effort":"medium"}], "default_reasoning_level":"medium"
        }]})
        .to_string(),
    )?;
    std::fs::write(
        root.join("openai-key-models.json"),
        json!({"data":[{
            "id":"gpt-6.1-sol", "max_model_len":64000,
            "supported_reasoning_levels":[{"effort":"high"}], "default_reasoning_level":"high"
        }]})
        .to_string(),
    )?;
    std::fs::write(
        root.join("claude-work-models.json"),
        r#"{"data":[{"id":"claude-opus-5"}]}"#,
    )?;
    Ok(root)
}

#[test]
fn connections_share_catalog_metadata_without_sharing_targets_or_capabilities() -> TestResult {
    let root = profile("connection-aliases")?;
    let published = PublishedConfig::load(&root)?;
    let id = |name| ProviderId::new(name).ok_or("test provider id");
    let chatgpt = published.issue_target_for(&id("chatgpt")?)?;
    let api = published.issue_target_for(&id("openai")?)?;
    let claude = published.issue_target_for(&id("claude-work")?)?;
    assert_eq!(chatgpt.provider().as_str(), "chatgpt");
    assert_eq!(chatgpt.account().as_str(), "subscription-account");
    assert_eq!(api.provider().as_str(), "openai");
    assert_eq!(api.account().as_str(), "key-account");
    assert_eq!(api.route().as_str(), "api-route");
    assert_eq!(claude.account().as_str(), "work-account");
    assert!(published.issue_target_for(&id("anthropic")?).is_err());
    let inventory = published.inventory().ok_or("missing inventory")?;
    let rows = inventory.available();
    let chatgpt = rows
        .iter()
        .find(|row| row.provider().as_str() == "chatgpt")
        .ok_or("ChatGPT row")?;
    let api = rows
        .iter()
        .find(|row| row.provider().as_str() == "openai")
        .ok_or("API row")?;
    let claude = rows
        .iter()
        .find(|row| row.provider().as_str() == "claude-work")
        .ok_or("Claude row")?;
    assert_eq!(chatgpt.context(), Some(258400));
    assert_eq!(api.context(), Some(64000));
    assert_eq!(
        chatgpt.default_variant().map(|level| level.as_str()),
        Some("medium")
    );
    assert_eq!(
        api.default_variant().map(|level| level.as_str()),
        Some("high")
    );
    assert!(claude.anthropic_thinking().is_some());
    assert!(!claude.variants().is_empty());
    assert_eq!(inventory.providers()["chatgpt"]["name"], "ChatGPT");
    assert_eq!(inventory.providers()["openai"]["name"], "API key");
    assert_eq!(inventory.providers()["claude-work"]["name"], "Work Claude");
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn malformed_provider_listing_never_falls_back_to_a_legacy_secondary_snapshot() -> TestResult {
    let root = profile("connection-shapes")?;
    let legacy = json!({"models":[{"slug":"gpt-6.1-sol", "context_window":64000,
        "supported_reasoning_levels":[]}]});
    std::fs::write(root.join("openai-key-models.json"), legacy.to_string())?;
    assert!(PublishedConfig::load(&root).is_ok());
    std::fs::write(
        root.join("openai-key-models.json"),
        legacy["models"].to_string(),
    )?;
    assert!(PublishedConfig::load(&root).is_ok());
    let mut malformed = legacy;
    malformed["data"] = json!(false);
    std::fs::write(root.join("openai-key-models.json"), malformed.to_string())?;
    assert!(PublishedConfig::load(&root).is_err());
    std::fs::remove_dir_all(root)?;
    Ok(())
}
