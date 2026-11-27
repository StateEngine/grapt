//! Published targets consume a complete connection generation, including trust.
use grapt_connections::store::{Connections, Store};
use grapt_platform::{profile_configuration, PublishedConfig};
use grapt_types::{AccountId, ProviderId};
use serde_json::json;
use std::path::PathBuf;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Profile(PathBuf);
impl Drop for Profile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn profile(label: &str) -> TestResult<(Profile, Connections)> {
    let path = std::env::temp_dir().join(format!(
        "grapt-connection-startup-{label}-{}",
        std::process::id()
    ));
    std::fs::create_dir(&path)?;
    grapt_login::profile_security::protect_directory(&path)?;
    let profile = Profile(std::fs::canonicalize(path)?);
    let mut store = Store::open(&profile.0)?;
    let mut records = store.migrate(
        json!({"provider":{
            "id":"openai", "model":"gpt-fixture", "account":"subscription-account",
            "route":"subscription-route", "credential_version":"chatgpt-oauth-v1",
            "policy_version":"policy", "protocol_family":"chatgpt-codex"
        }})
        .to_string()
        .as_bytes(),
    )?;
    let mut api = records
        .connections
        .first()
        .ok_or("migration was empty")?
        .clone();
    let chatgpt = ProviderId::new("chatgpt").ok_or("fixture provider")?;
    records.connections[0].id = chatgpt.clone();
    records
        .default_model
        .as_mut()
        .ok_or("migration selection")?
        .provider = chatgpt;
    api.family = grapt_connections::store::Family::OpenaiResponses;
    api.method = grapt_connections::store::AuthMethod::Key;
    api.account = AccountId::new("api-account").ok_or("fixture account")?;
    api.label = "API key".into();
    api.credential_file = "openai-key.credential".into();
    api.discovery_file = "openai-key-models.json".into();
    api.oauth_file = None;
    records.connections.push(api);
    store.save(&records)?;
    std::fs::write(
        profile.0.join("grapt.jsonc"),
        json!({
            "store":{"authority":"open-ready"}, "product":{"build":"connection-startup"},
            "provider":{"id":"ignored-invalid-legacy"}
        })
        .to_string(),
    )?;
    std::fs::write(
        profile.0.join("model-catalog.json"),
        json!({"openai":{
            "id":"openai", "name":"OpenAI", "models":{"gpt-fixture":{
                "id":"gpt-fixture", "name":"GPT", "limit":{"context":64000}
            }}
        }})
        .to_string(),
    )?;
    std::fs::write(
        profile.0.join("account-models.json"),
        json!({"models":[{
            "slug":"gpt-fixture", "context_window":64000, "supported_reasoning_levels":[]
        }]})
        .to_string(),
    )?;
    std::fs::write(
        profile.0.join("openai-key-models.json"),
        r#"{"data":[{"id":"gpt-fixture"}]}"#,
    )?;
    Ok((profile, records))
}

fn account(published: &PublishedConfig, provider: &str) -> TestResult<String> {
    Ok(published
        .issue_target_for(&ProviderId::new(provider).ok_or("fixture provider")?)?
        .account()
        .as_str()
        .to_owned())
}

#[test]
fn restart_reads_connections_and_user_selection_without_reimporting_legacy() -> TestResult {
    let (profile, mut records) = profile("explicit")?;
    let published = PublishedConfig::load(&profile.0)?;
    assert_eq!(account(&published, "chatgpt")?, "subscription-account");
    assert_eq!(account(&published, "openai")?, "api-account");
    assert_eq!(published.target()?.provider().as_str(), "chatgpt");
    std::fs::write(
        profile.0.join("grapt.jsonc"),
        json!({
            "store":{"authority":"open-ready"}, "product":{"build":"connection-startup"},
            "default_model":{"providerID":"openai", "id":"gpt-fixture"}
        })
        .to_string(),
    )?;
    records.connections[0].account =
        AccountId::new("replacement-account").ok_or("fixture account")?;
    Store::open(&profile.0)?.save(&records)?;
    let restarted = PublishedConfig::load(&profile.0)?;
    assert_eq!(restarted.target()?.provider().as_str(), "openai");
    assert_eq!(account(&restarted, "chatgpt")?, "replacement-account");
    assert_eq!(account(&published, "chatgpt")?, "subscription-account");
    for listing in [
        json!({"models":[{"slug":"gpt-fixture", "context_window":64000,
            "supported_reasoning_levels":[]}]}),
        json!([{"slug":"gpt-fixture", "context_window":64000,
            "supported_reasoning_levels":[]}]),
    ] {
        std::fs::write(
            profile.0.join("openai-key-models.json"),
            listing.to_string(),
        )?;
        assert_eq!(
            PublishedConfig::load(&profile.0)?
                .target()?
                .provider()
                .as_str(),
            "openai"
        );
    }
    std::fs::write(profile.0.join("connections.json"), b"damaged")?;
    assert!(PublishedConfig::load(&profile.0).is_err());
    Ok(())
}

#[test]
fn migrated_api_profiles_keep_optional_catalog_metadata() -> TestResult {
    let (profile, mut records) = profile("api-without-catalog")?;
    records
        .connections
        .retain(|connection| connection.id.as_str() == "openai");
    records.default_model.as_mut().ok_or("selection")?.provider =
        ProviderId::new("openai").ok_or("fixture provider")?;
    Store::open(&profile.0)?.save(&records)?;
    std::fs::remove_file(profile.0.join("model-catalog.json"))?;
    // Obsolete legacy account/routing fields are no longer required. Only
    // matching model metadata supplies the listing's missing family.
    std::fs::write(
        profile.0.join("grapt.jsonc"),
        json!({
            "store":{"authority":"open-ready"}, "product":{"build":"connection-startup"},
            "provider":{"id":"openai", "model":"gpt-fixture", "base_model":"gpt-fixture",
                "family":"configured-family"}
        })
        .to_string(),
    )?;
    let published = PublishedConfig::load(&profile.0)?;
    assert_eq!(
        published
            .inventory()
            .ok_or("inventory")?
            .available()
            .first()
            .ok_or("available model")?
            .metadata()["family"],
        "configured-family"
    );
    let mut settings = json!({
        "store":{"authority":"open-ready"}, "product":{"build":"connection-startup"},
        "provider":{"id":"openai", "model":"gpt-fixture"}
    });
    std::fs::write(profile.0.join("grapt.jsonc"), settings.to_string())?;
    assert!(PublishedConfig::load(&profile.0).is_ok());
    settings["provider"]["family"] = json!(42);
    std::fs::write(profile.0.join("grapt.jsonc"), settings.to_string())?;
    assert!(PublishedConfig::load(&profile.0).is_err());
    assert_eq!(published.target()?.provider().as_str(), "openai");
    assert_eq!(account(&published, "openai")?, "api-account");
    assert!(published
        .inventory()
        .ok_or("inventory")?
        .available()
        .iter()
        .any(|model| model.id().as_str() == "gpt-fixture"));
    Ok(())
}

#[test]
fn repository_approval_binds_consumption_to_the_captured_connection_generation() -> TestResult {
    let (profile, mut records) = profile("approved")?;
    let approved = profile_configuration::ProfileConfiguration::capture(&profile.0)?
        .approval()
        .clone();
    records.connections[0].account = AccountId::new("foreign-account").ok_or("fixture account")?;
    Store::open(&profile.0)?.save(&records)?;
    assert!(profile_configuration::install(&profile.0, &approved).is_err());
    records.connections[0].account =
        AccountId::new("subscription-account").ok_or("fixture account")?;
    Store::open(&profile.0)?.save(&records)?;
    profile_configuration::install(&profile.0, &approved)?;
    records.connections[0].account = AccountId::new("later-account").ok_or("fixture account")?;
    Store::open(&profile.0)?.save(&records)?;
    assert_eq!(
        account(&PublishedConfig::load(&profile.0)?, "chatgpt")?,
        "subscription-account"
    );
    Ok(())
}

#[test]
fn legacy_bootstrap_keeps_selection_and_refuses_to_replace_a_concurrent_generation() -> TestResult {
    let (profile, _) = profile("first-touch")?;
    std::fs::remove_file(profile.0.join("connections.json"))?;
    std::fs::remove_file(profile.0.join("account-models.json"))?;
    std::fs::remove_file(profile.0.join("model-catalog.json"))?;
    let legacy = json!({
        "store":{"authority":"open-ready"}, "product":{"build":"legacy-startup"},
        "provider":{"id":"openai","model":"gpt-fixture","base_model":"gpt-fixture","family":"gpt",
            "account":"subscription-account","route":"subscription-route","credential_version":"chatgpt-oauth-v1",
            "policy_version":"policy","variants":[]}
    }).to_string();
    std::fs::write(profile.0.join("grapt.jsonc"), &legacy)?;
    let published = PublishedConfig::load(&profile.0)?;
    assert!(!profile.0.join("connections.json").exists());
    assert!(published.ensure_connection_store()?);
    assert!(!published.ensure_connection_store()?);
    let mut records = Store::read_snapshot(&profile.0)?.ok_or("missing migrated store")?;
    assert_eq!(
        records
            .default_model
            .as_ref()
            .ok_or("selection")?
            .provider
            .as_str(),
        "openai"
    );
    assert_eq!(
        account(&PublishedConfig::load(&profile.0)?, "openai")?,
        "subscription-account"
    );
    assert_eq!(
        std::fs::read(profile.0.join("grapt.jsonc"))?,
        legacy.as_bytes()
    );
    records.connections.first_mut().ok_or("connection")?.account =
        AccountId::new("concurrent-account").ok_or("account")?;
    Store::open(&profile.0)?.save(&records)?;
    assert!(published.ensure_connection_store().is_err());
    assert_eq!(Store::read_snapshot(&profile.0)?, Some(records));
    Ok(())
}
