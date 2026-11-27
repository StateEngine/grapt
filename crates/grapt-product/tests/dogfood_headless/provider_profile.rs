//! Disposable service profile configuration for headless journeys.

use super::*;

#[path = "../../../../test-support/executable.rs"]
mod executable;

/// Runs this root's Sessions at the `author` preset, where a subagent, a
/// skill or a plugin tool still asks: for witnesses of that ask. Under the
/// default `maintainer` those run without asking.
pub(super) fn attended(root: &Path) {
    let config = root.join("service.conf");
    let mut text = fs::read_to_string(&config)
        .unwrap_or_else(|error| panic!("service config unreadable: {error}"));
    text.push_str("permission_preset=author\n");
    fs::write(&config, text).unwrap_or_else(|error| panic!("service config failed: {error}"));
}

pub(super) fn prepare(root: &Path, window_ms: u64) {
    // Load the child before any provider latch or readiness allowance starts.
    // The 30s readiness bound and replay/live/resume assertions stay intact.
    static PREPARED: std::sync::OnceLock<Result<u64, String>> = std::sync::OnceLock::new();
    PREPARED
        .get_or_init(|| executable::prepare(&shipped_cli::product_binary()))
        .as_ref()
        .unwrap_or_else(|error| panic!("product {error}"));
    if root.exists() {
        fs::remove_dir_all(root)
            .unwrap_or_else(|error| panic!("{} reset failed: {error}", root.display()));
    }
    fs::create_dir_all(root)
        .unwrap_or_else(|error| panic!("{} creation failed: {error}", root.display()));
    // This profile supplies user-owned authority, so it must stay outside a
    // repository. Git-specific journeys create their own project worktrees.
    let database = root.join("database.sqlite3");
    let store = EventStore::open(&database, "live-headless1")
        .unwrap_or_else(|error| panic!("store creation failed: {error}"));
    drop(store);
    let migrations = ProductionMigrations::load()
        .unwrap_or_else(|error| panic!("migration load failed: {error}"));
    match apply_migrations(&database, root, "0.1.0", &migrations.as_slice()) {
        Ok(MigrationOutcome::Applied { .. } | MigrationOutcome::AlreadyCurrent) => {}
        Err(error) => panic!("migration failed: {error}"),
    }
    fs::write(
        root.join("service.conf"),
        format!("slot=product\ninstance=live-headless1\nenvironment=acceptance\nversion=0.1.0\nserve_window_ms={window_ms}\nprovider_mode=production\nprovider_connect_ms=10000\nprovider_response_head_ms=30000\nprovider_active_attempts=64\n"),
    )
    .unwrap_or_else(|error| panic!("service config failed: {error}"));
    fs::write(root.join("basic.credential"), "alpha:zephyr")
        .unwrap_or_else(|error| panic!("basic credential failed: {error}"));
    fs::write(root.join("provider.credential"), "provider-secret")
        .unwrap_or_else(|error| panic!("provider credential failed: {error}"));
    fs::write(
        root.join("grapt.jsonc"),
        r#"{
          "store": { "authority": "open-ready" },
          "product": { "build": "live-headless1-build" },
          "provider": {
            "id": "openai", "model": "gpt-live", "base_model": "gpt-live",
            "family": "openai-responses", "route": "route-live", "account": "account-a",
            "credential_version": "credential-v1", "policy_version": "live-policy-v1",
            "variants": []
          },
          "compaction": {},
          "lsp": false
        }"#,
    )
    .unwrap_or_else(|error| panic!("platform config failed: {error}"));
    prepare_model_inventory(root, 128_000);
}

/// Merge the named legacy/additional compaction controls into the fixture config.
pub(super) fn set_compaction(root: &Path, keys: serde_json::Value) {
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(
        &fs::read(&path).unwrap_or_else(|error| panic!("config read failed: {error}")),
    )
    .unwrap_or_else(|error| panic!("config decode failed: {error}"));
    for (key, value) in keys
        .as_object()
        .unwrap_or_else(|| panic!("compaction keys must be an object"))
    {
        // The model limit is a root key of the config, beside `compaction`.
        if key == "model_auto_compact_token_limit" {
            config[key] = value.clone();
            continue;
        }
        let compaction = config["compaction"]
            .as_object_mut()
            .unwrap_or_else(|| panic!("the fixture config has no compaction object"));
        compaction.insert(key.clone(), value.clone());
    }
    fs::write(&path, serde_json::to_vec(&config).unwrap_or_default())
        .unwrap_or_else(|error| panic!("config write failed: {error}"));
}

/// Supply the explicit bounded inventory required by context admission.
///
/// Every headless journey that can reach an attempt must name a real discovered
/// capacity rather than inheriting an absent value as zero. Individual tests may
/// replace this inventory when capacity itself is the behavior under test.
pub(super) fn prepare_model_inventory(root: &Path, context_window: u64) {
    let config_path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(
        &fs::read(&config_path)
            .unwrap_or_else(|error| panic!("platform config read failed: {error}")),
    )
    .unwrap_or_else(|error| panic!("platform config decode failed: {error}"));
    config["catalog"] = serde_json::json!({"source": "models.dev-chatgpt"});
    fs::write(
        &config_path,
        serde_json::to_vec(&config)
            .unwrap_or_else(|error| panic!("platform config encode failed: {error}")),
    )
    .unwrap_or_else(|error| panic!("platform config update failed: {error}"));
    fs::write(
        root.join("model-catalog.json"),
        br#"{"openai":{"id":"openai","name":"OpenAI","npm":"@ai-sdk/openai","models":{"gpt-live":{"id":"gpt-live","name":"gpt-live","family":"gpt-5","tool_call":true,"modalities":{"input":["text"],"output":["text"]},"limit":{"output":32000}}}}}"#,
    )
    .unwrap_or_else(|error| panic!("model catalog fixture failed: {error}"));
    fs::write(
        root.join("account-models.json"),
        serde_json::to_vec(&serde_json::json!({
            "models": [{
                "slug": "gpt-live",
                "visibility": "list",
                "context_window": context_window,
                "supported_reasoning_levels": []
            }]
        }))
        .unwrap_or_else(|error| panic!("model inventory encode failed: {error}")),
    )
    .unwrap_or_else(|error| panic!("model inventory fixture failed: {error}"));
}
