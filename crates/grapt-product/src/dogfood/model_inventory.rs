//! Retained catalog DTOs projected from platform-owned discovery data.
use super::CatalogProjection;
use serde_json::{json, Value};

/// The CONNECTED providers, which is what this listing has always meant to the
/// retained frontend.
///
/// The frozen renderer reads this array as the connected set and seeds its
/// provider map from it (`connected: providers.map((p) => p.id)` and the `all`
/// seeding above it, in packages/app/src/context/global-sync/utils.ts), then
/// DROPS any model whose `providerID` is not in that map. Emitting the whole
/// public catalog here therefore did not add breadth; it reported every one of
/// the catalog's providers as connected, which is why an unconfigured provider
/// was previously indistinguishable from a configured one. Catalog metadata is
/// still read for the rows below — reading it connects nothing.
pub(super) fn providers(catalog: &CatalogProjection) -> Value {
    if catalog.provider.is_empty() {
        return json!([]);
    }
    match &catalog.inventory {
        Some(inventory) => Value::Array(inventory.connected().iter().filter_map(|provider| {
            let row = inventory.providers().get(provider.as_str())?;
            let mut value = json!({"id":provider.as_str(),"name":row["name"],"package":format!("aisdk:{}", row["npm"].as_str().unwrap_or(""))});
            if let Some(api) = row.get("api") { value["settings"] = json!({"baseURL":api}); }
            Some(with_account(value, catalog))
        }).collect()),
        None => json!([with_account(json!({"id":catalog.provider,"name":catalog.provider,"package":"builtin:live"}), catalog)]),
    }
}

/// Display labels follow the same published provider/account binding as execution.
fn with_account(mut value: Value, catalog: &CatalogProjection) -> Value {
    let account = value
        .get("id")
        .and_then(Value::as_str)
        .and_then(|provider| {
            catalog
                .accounts
                .iter()
                .find(|(id, _)| id.as_str() == provider)
                .map(|(_, account)| account)
        });
    if let Some(account) = account {
        if value.get("settings").is_none() {
            value["settings"] = json!({});
        }
        value["settings"]["account"] = json!(account.as_str());
    }
    value
}

pub(super) fn models(catalog: &CatalogProjection) -> Value {
    if catalog.provider.is_empty() {
        return json!([]);
    }
    match &catalog.inventory {
        Some(inventory) => Value::Array(inventory.available().iter().map(|model| {
            let row = model.metadata();
            let variants: Vec<Value> = model.variants().iter().map(|effort| json!({
                "id":effort.as_str()
            })).collect();
            let mut value = json!({
                // Each row carries the provider it is actually issuable
                // through. Stamping the default connection's id here would
                // attach a second account's models to the wrong provider, and
                // the renderer would then route their selection to the wrong
                // connection.
                "id":model.id().as_str(),"modelID":model.id().as_str(),"providerID":model.provider().as_str(),
                "name":row["name"],
                "capabilities":wire_capabilities(row),
                "variants":variants,"time":{"released":released(row.get("release_date").and_then(Value::as_str))},
                // The frozen ChatGPT subscription integration clears public
                // API prices. These are catalog prices, not observed billing.
                "cost":[],"status":row.get("status").and_then(Value::as_str).unwrap_or("active"),"enabled":true,
                "limit":{"context":model.context().unwrap_or(0),"input":model.context().unwrap_or(0),"output":wire_output_limit(row)}
            });
            if row["metadata_source"] == "provider-listing" {
                // Only fields the listing did not report stay unknown; a
                // reported capability is a fact even on a listing row.
                let mut unknown = Vec::new();
                if row["tool_call"].is_null() { unknown.push("tools"); }
                if row["modalities"]["input"].is_null() { unknown.push("input"); }
                if row["modalities"]["output"].is_null() { unknown.push("output"); }
                if row["limit"]["output"].is_null() { unknown.push("output_limit"); }
                if model.context().is_none() { unknown.push("context"); }
                value["settings"] = json!({"metadata":{"source":"provider-listing","unknown":unknown}});
            }
            // Expose the effort inherited by an unset
            // selection without turning it into an explicit session choice.
            let effort = if model.provider().as_str() == catalog.provider && model.id().as_str() == catalog.model {
                catalog.default_effort.as_ref()
            } else {
                model.default_variant()
            };
            if let Some(effort) = effort {
                if value.get("settings").is_none() { value["settings"] = json!({"metadata":{}}); }
                value["settings"]["metadata"]["default_effort"] = json!(effort.as_str());
            }
            if let Some(family) = row.get("family") { value["family"] = family.clone(); }
            value
        }).collect()),
        None => json!([super::support::model(catalog)]),
    }
}

/// The retained DTO uses false/empty arrays for unreported capabilities.
/// Reported fields are carried through even on a provider-listing row; only
/// unreported ones take the unknown spelling, kept explicit in settings.
fn wire_capabilities(metadata: &Value) -> Value {
    let known = |reported: &Value, unknown: Value| {
        if reported.is_null() {
            unknown
        } else {
            reported.clone()
        }
    };
    json!({
        "tools":known(&metadata["tool_call"], json!(false)),
        "input":known(&metadata["modalities"]["input"], json!([])),
        "output":known(&metadata["modalities"]["output"], json!([]))
    })
}

/// The retained DTO requires an integer output limit. Preserve its numeric
/// unknown-limit convention here, while canonical account metadata keeps an
/// unreported limit absent. A present invalid value stays invalid.
fn wire_output_limit(metadata: &Value) -> Value {
    let Some(limit) = metadata["limit"].as_object() else {
        return Value::Null;
    };
    match limit.get("output") {
        Some(Value::Null) if metadata["metadata_source"] == "provider-listing" => json!(0),
        Some(output) => output.clone(),
        None if metadata["metadata_source"] == "account-listing" => json!(0),
        None => Value::Null,
    }
}

#[cfg(test)]
mod output_limit_tests {
    use super::*;

    #[test]
    fn an_unreported_account_limit_is_normalized_only_at_the_wire_boundary() {
        assert_eq!(
            wire_output_limit(&json!({"limit":{},"metadata_source":"account-listing"})),
            json!(0)
        );
        assert_eq!(wire_output_limit(&json!({"limit":{"output":0}})), json!(0));
        assert_eq!(
            wire_output_limit(&json!({"limit":{"output":64000}})),
            json!(64000)
        );
        for value in [Value::Null, json!("unreadable")] {
            assert_eq!(
                wire_output_limit(
                    &json!({"limit":{"output":value},"metadata_source":"account-listing"})
                ),
                value
            );
        }
        for limit in [Value::Null, json!("unreadable")] {
            assert_eq!(
                wire_output_limit(&json!({"limit":limit,"metadata_source":"account-listing"})),
                Value::Null,
                "a malformed limit object is not an unknown capacity"
            );
        }
        assert_eq!(wire_output_limit(&json!({"limit":{}})), Value::Null);
    }
}

#[cfg(test)]
mod listing_capability_tests {
    use super::*;

    #[test]
    fn review_out_of_range_optional_output_cannot_make_the_model_dto_unreadable(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let inventory = grapt_platform::ModelInventory::first_run(
            json!({"openai":{"id":"openai","models":{}}}),
            json!({"data":[{"id":"vendor/model","max_model_len":500000,
                "max_completion_tokens":u64::MAX}]}),
            "openai",
            "models.dev-openai",
            "openai-responses",
        )?;
        let catalog = CatalogProjection {
            accounts: Default::default(),
            protocols: Default::default(),
            inventory: Some(std::sync::Arc::new(inventory)),
            project: grapt_types::ProjectId::new("project-a").ok_or("project id")?,
            directory: "/work/project".to_owned(),
            provider: "openai".to_owned(),
            model: "vendor/model".to_owned(),
            default_effort: None,
            credential_record: None,
        };
        let decoded: Vec<grapt_api::u3a::ModelInfoDto> = serde_json::from_value(models(&catalog))?;
        assert_eq!(decoded.len(), 1);
        Ok(())
    }

    #[test]
    fn a_listing_row_reports_only_the_capabilities_it_actually_states() -> Result<(), String> {
        let mut catalog = CatalogProjection {
            accounts: Default::default(),
            protocols: Default::default(),
            inventory: None,
            project: grapt_types::ProjectId::new("project-a").ok_or("project id")?,
            directory: "/work/project".to_owned(),
            provider: "custom".to_owned(),
            model: "vendor/model".to_owned(),
            default_effort: None,
            credential_record: None,
        };
        let inventory = grapt_platform::ModelInventory::first_run(
            json!({"openai":{"id":"openai","models":{}}}),
            json!({"data":[{"id":"vendor/model","max_model_len":500000,
            "context_length":131072,"max_completion_tokens":16384,
            "supported_parameters":["tools","temperature"],
            "input_modalities":["text"],"output_modalities":["text"]}]}),
            "openai",
            "models.dev-openai",
            "openai-responses",
        )?;
        catalog.inventory = Some(std::sync::Arc::new(inventory));
        catalog.provider = "openai".to_owned();
        let rows = models(&catalog);
        let row = &rows.as_array().ok_or("model rows")?[0];
        assert_eq!(row["capabilities"]["tools"], true);
        assert_eq!(row["capabilities"]["input"], json!(["text"]));
        assert_eq!(row["limit"]["output"], 16384);
        assert_eq!(row["limit"]["context"], 131072);
        assert_eq!(row["settings"]["metadata"]["unknown"], json!([]));

        // A row that reports nothing keeps every capability unknown, and the
        // retained DTO's zero/false spelling stays marked as unknown.
        let bare = grapt_platform::ModelInventory::first_run(
            json!({"openai":{"id":"openai","models":{}}}),
            json!({"data":[{"id":"vendor/model"}]}),
            "openai",
            "models.dev-openai",
            "openai-responses",
        )?;
        catalog.inventory = Some(std::sync::Arc::new(bare));
        let rows = models(&catalog);
        let row = &rows.as_array().ok_or("model rows")?[0];
        assert_eq!(row["capabilities"]["tools"], false);
        assert_eq!(row["limit"]["output"], 0);
        assert_eq!(
            row["settings"]["metadata"]["unknown"],
            json!(["tools", "input", "output", "output_limit", "context"])
        );
        Ok(())
    }
}

pub(super) fn default_model(catalog: &CatalogProjection) -> Value {
    let values = models(catalog);
    values
        .as_array()
        .and_then(|values| {
            values.iter().find(|value| {
                value["id"].as_str() == Some(catalog.model.as_str())
                    && value["providerID"].as_str() == Some(catalog.provider.as_str())
            })
        })
        .cloned()
        .unwrap_or(Value::Null)
}

/// Resolve the legacy OpenAI name to a connected ChatGPT subscription before
/// persisting the selection. An explicit OpenAI connection always owns its name,
/// even when it does not offer this model; aliases never change accounts there.
pub(super) fn resolve(catalog: &CatalogProjection, selected: &Value) -> Option<Value> {
    if accepts(catalog, selected) {
        return Some(selected.clone());
    }
    let inventory = catalog.inventory.as_ref()?;
    if selected.get("providerID")?.as_str()? != "openai"
        || inventory
            .connected()
            .iter()
            .any(|id| id.as_str() == "openai")
        || !catalog.protocols.iter().any(|(id, family)| {
            id.as_str() == "chatgpt" && *family == grapt_provider::ProtocolFamily::ChatGptCodex
        })
    {
        return None;
    }
    let mut canonical = selected.clone();
    canonical["providerID"] = json!("chatgpt");
    accepts(catalog, &canonical).then_some(canonical)
}

/// Whether a selection names a row this composition can actually issue.
///
/// The whole `(provider, model, variant)` coordinate must match one connected
/// row. Matching the model alone would let a selection carrying an unconfigured
/// or simply wrong provider be accepted and then issued through whichever
/// account happened to own that model id.
pub(super) fn accepts(catalog: &CatalogProjection, selected: &Value) -> bool {
    if catalog.provider.is_empty() {
        return false;
    }
    let Some(provider) = selected.get("providerID").and_then(Value::as_str) else {
        return false;
    };
    let id = selected.get("id").and_then(Value::as_str);
    let variant = selected
        .get("variant")
        .and_then(Value::as_str)
        .filter(|value| *value != "default");
    match &catalog.inventory {
        Some(inventory) => inventory.available().iter().any(|row| {
            row.provider().as_str() == provider
                && Some(row.id().as_str()) == id
                && variant.is_none_or(|value| {
                    row.variants().iter().any(|effort| effort.as_str() == value)
                })
        }),
        None => {
            provider == catalog.provider.as_str()
                && id == Some(catalog.model.as_str())
                && variant.is_none()
        }
    }
}

// Gregorian civil date to epoch milliseconds; no clock or locale dependency.
fn released(value: Option<&str>) -> f64 {
    let Some(value) = value else {
        return 0.0;
    };
    let parts: Vec<&str> = value.split('-').take(4).collect();
    let [year, month, day] = parts.as_slice() else {
        return 0.0;
    };
    let (Ok(mut year), Ok(month), Ok(day)) = (
        year.parse::<i64>(),
        month.parse::<i64>(),
        day.parse::<i64>(),
    ) else {
        return 0.0;
    };
    if !(1970..=9999).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return 0.0;
    }
    year -= i64::from(month <= 2);
    let era = year / 400;
    let yoe = year - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
    (days * 86_400_000) as f64
}

#[cfg(test)]
mod tests {
    //! J01 — what the retained frontend is actually told about providers.
    //!
    //! The frozen renderer treats this provider array as the CONNECTED set and
    //! drops any model whose `providerID` is not in it
    //! (packages/app/src/context/global-sync/utils.ts). These witnesses pin both
    //! halves of that contract against a real composed inventory rather than a
    //! hand-built stub, so a projection that quietly reports the whole catalog
    //! as connected, or stamps one provider onto every row, cannot pass.
    use super::*;
    use grapt_types::ProjectId;

    const CATALOG: &str = r#"{
  "openai":{"id":"openai","name":"OpenAI","npm":"@ai-sdk/openai","models":{
    "gpt-5.6":{"id":"gpt-5.6","name":"GPT-5.6","family":"gpt-5",
      "tool_call":true,"modalities":{"input":["text"],"output":["text"]},
      "limit":{"output":64000}}
  }},
  "openrouter":{"id":"openrouter","name":"OpenRouter","npm":"@openrouter/ai-sdk-provider","models":{
    "vendor-model-a":{"id":"vendor-model-a","name":"Model A","family":"model-a",
      "tool_call":true,"modalities":{"input":["text"],"output":["text"]},
      "limit":{"output":32000}}
  }},
  "unconfigured":{"id":"unconfigured","name":"Unconfigured","npm":"x","models":{
    "never":{"id":"never","name":"Never","family":"never",
      "tool_call":false,"modalities":{"input":["text"],"output":["text"]},
      "limit":{"output":1000}}
  }}
}"#;

    #[tokio::test]
    async fn configured_secondary_opener_binds_its_own_route_and_credential(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use grapt_provider::{
            OpenAiCompatibleRequest, PhysicalAttemptAuthoritySnapshot, ProviderRequest,
        };
        let (scratch, _) = composed("secondary-opener");
        std::fs::write(
            scratch.0.join("grapt.jsonc"),
            CONFIG.replace("open-ready", "initialize"),
        )?;
        // Initial schema creation and governed migration are separate first contacts.
        let first = grapt_platform::PlatformOwner::prepare(&scratch.0);
        let platform = match first {
            Ok(platform) => platform,
            Err(_) if scratch.0.join("database.sqlite3").is_file() => {
                std::fs::write(
                    scratch.0.join("grapt.jsonc"),
                    CONFIG.replace("open-ready", "migrate"),
                )?;
                grapt_platform::PlatformOwner::prepare(&scratch.0)?
            }
            Err(error) => return Err(error.into()),
        };
        let mut parts = platform.into_provider_parts()?;
        let secondary = parts.take_secondary_credentials();
        let (_store, targets, _, credential, _) = parts.into_components();
        let (dispatch, _samples) = grapt_provider::DispatchObserver::bounded(std::sync::Arc::new(
            grapt_store::SystemClock,
        ));
        let limits = super::super::production::ProductionProviderLimits {
            connect: std::time::Duration::from_secs(1),
            response_head: std::time::Duration::from_secs(1),
            active_attempts: std::num::NonZeroUsize::new(1).ok_or("zero limit")?,
            child_attempts: std::num::NonZeroUsize::new(3).ok_or("zero child limit")?,
        };
        let (_first, openers) = super::super::execution::providers::open(
            &targets,
            credential,
            secondary,
            &super::super::LiveEndpoint::Production(grapt_provider::EndpointConfig::new()),
            limits,
            dispatch,
            grapt_provider::ProviderErrorText::disabled(),
            std::sync::Arc::new(grapt_store::SystemClock),
        )?;
        assert_eq!(openers.len(), 2);
        let provider = grapt_types::ProviderId::new("openrouter").ok_or("provider")?;
        let model = grapt_types::ModelId::new("vendor-model-a").ok_or("model")?;
        for (opener_provider, succeeds) in [("openrouter", true), ("openai", false)] {
            let target = targets.issue_selected(&provider, &model, None)?;
            let mut authority = PhysicalAttemptAuthoritySnapshot::builder();
            authority.model(target)?;
            let authority = authority.build()?;
            let request = ProviderRequest::OpenAiCompatible(OpenAiCompatibleRequest {
                reasoning_replay: Default::default(),
                prompt_cache: grapt_provider::CompatiblePromptCache::Off,
                model: authority.model().model_ref().clone(),
                messages: vec![],
                max_tokens: None,
                tools: vec![],
                tool_choice: None,
                routing_session: None,
            });
            let opener_id =
                grapt_types::ProviderId::new(opener_provider).ok_or("opener provider")?;
            let opener = openers.get(&opener_id).ok_or("opener missing")?;
            let attempt = grapt_types::PhysicalAttemptId::new(format!("pat_{opener_provider}"))
                .ok_or("attempt")?;
            let result = opener.open(attempt, authority, request);
            assert_eq!(
                result.is_ok(),
                succeeds,
                "wrong route or credential bound: {result:?}"
            );
        }
        Ok(())
    }

    const CONFIG: &str = r#"{
  "store":{"authority":"open-ready"},
  "product":{"build":"build-a"},
  "catalog":{"source":"models.dev-chatgpt"},
  "provider":{
    "id":"openai","model":"gpt-5.6","base_model":"gpt-5.6","family":"gpt-5",
    "route":"responses","account":"account-a",
    "credential_version":"credential-v1","policy_version":"df0-policy-v1",
    "accounts":[
      {"id":"openrouter","route":"route-openrouter","account":"account-b",
       "credential_version":"credential-b1","policy_version":"df0-policy-v1",
       "protocol_family":"openai-compatible",
       "endpoint":"https://openrouter.ai/api/v1",
       "credential":"openrouter.credential",
       "discovery":"openrouter-account-models.json"}
    ]
  }
}"#;

    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn composed(label: &str) -> (Scratch, CatalogProjection) {
        let root =
            std::env::temp_dir().join(format!("grapt-dub-product-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let write = |name: &str, body: &str| {
            std::fs::write(root.join(name), body)
                .unwrap_or_else(|error| panic!("{name} fixture is unwritable: {error}"));
        };
        std::fs::create_dir_all(&root)
            .unwrap_or_else(|error| panic!("scratch root is unusable: {error}"));
        write("grapt.jsonc", CONFIG);
        write("model-catalog.json", CATALOG);
        write("provider.credential", "sentinel-provider-secret\n");
        write("openrouter.credential", "sentinel-openrouter-secret\n");
        write(
            "account-models.json",
            r#"{"models":[{"slug":"gpt-5.6","visibility":"list","context_window":272000,
                "supported_reasoning_levels":[]}]}"#,
        );
        write(
            "openrouter-account-models.json",
            r#"{"models":[{"slug":"vendor-model-a","visibility":"list","context_window":128000,
                "supported_reasoning_levels":[]}]}"#,
        );
        let published = grapt_platform::PublishedConfig::load(&root)
            .unwrap_or_else(|error| panic!("the composed configuration was refused: {error}"));
        let inventory = published
            .inventory()
            .unwrap_or_else(|| panic!("a catalog source must compose an inventory"));
        let catalog = CatalogProjection {
            accounts: Default::default(),
            protocols: Default::default(),
            inventory: Some(inventory),
            project: ProjectId::new("project-a")
                .unwrap_or_else(|| panic!("fixture project id is invalid")),
            directory: "/work/project".to_owned(),
            provider: "openai".to_owned(),
            model: "gpt-5.6".to_owned(),
            default_effort: None,
            credential_record: None,
        };
        (Scratch(root), catalog)
    }

    fn chatgpt_catalog(
        label: &str,
        api: bool,
    ) -> Result<(Scratch, CatalogProjection), Box<dyn std::error::Error>> {
        let (scratch, mut catalog) = composed(label);
        let mut config: Value = serde_json::from_str(CONFIG)?;
        config["provider"]["id"] = json!("chatgpt");
        config["provider"]["catalog_provider"] = json!("openai");
        if api {
            config["provider"]["accounts"].as_array_mut().ok_or("accounts")?.push(json!({
                "id":"openai", "catalog_provider":"openai", "route":"api-route",
                "account":"api-account", "credential_version":"api-v1", "policy_version":"policy-v1",
                "protocol_family":"openai-responses", "credential":"api.credential", "discovery":"api-models.json"
            }));
            std::fs::write(scratch.0.join("api.credential"), "api-fixture-secret")?;
            std::fs::write(
                scratch.0.join("api-models.json"),
                r#"{"data":[{"id":"api-only","max_model_len":64000}]}"#,
            )?;
        }
        std::fs::write(scratch.0.join("grapt.jsonc"), serde_json::to_vec(&config)?)?;
        catalog.inventory = grapt_platform::PublishedConfig::load(&scratch.0)?.inventory();
        catalog.provider = "chatgpt".into();
        catalog.protocols.insert(
            grapt_types::ProviderId::new("chatgpt").ok_or("provider")?,
            grapt_provider::ProtocolFamily::ChatGptCodex,
        );
        Ok((scratch, catalog))
    }

    #[test]
    fn legacy_openai_selection_resolves_only_an_offered_chatgpt_model(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_scratch, mut catalog) = chatgpt_catalog("legacy-openai-alias", false)?;
        let legacy = json!({"providerID":"openai","id":"gpt-5.6","variant":"default"});
        let canonical = resolve(&catalog, &legacy).ok_or("legacy model refused")?;
        assert_eq!(
            canonical,
            json!({"providerID":"chatgpt","id":"gpt-5.6","variant":"default"})
        );
        assert_eq!(resolve(&catalog, &canonical), Some(canonical));
        for selected in [
            json!({"providerID":"openai","id":"absent"}),
            json!({"providerID":"openai","id":"gpt-5.6","variant":"unsupported"}),
            json!({"providerID":"other","id":"gpt-5.6"}),
            json!({"id":"gpt-5.6"}),
        ] {
            assert!(
                resolve(&catalog, &selected).is_none(),
                "unavailable selection accepted: {selected}"
            );
        }
        catalog.protocols.clear();
        assert!(
            resolve(&catalog, &legacy).is_none(),
            "a connection named chatgpt without the ChatGPT protocol became an alias"
        );
        Ok(())
    }

    #[test]
    fn an_explicit_openai_connection_never_falls_back_to_chatgpt(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_scratch, catalog) = chatgpt_catalog("explicit-openai-account", true)?;
        let api = json!({"providerID":"openai","id":"api-only"});
        assert_eq!(resolve(&catalog, &api), Some(api));
        assert!(resolve(&catalog, &json!({"providerID":"openai","id":"gpt-5.6"})).is_none());
        let subscription = json!({"providerID":"chatgpt","id":"gpt-5.6"});
        assert_eq!(resolve(&catalog, &subscription), Some(subscription));
        Ok(())
    }

    fn ids(value: &Value) -> Vec<String> {
        value
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| row["id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn inherited_effort_projects_without_becoming_a_session_variant(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_scratch, mut catalog) = composed("default-effort");
        catalog.default_effort = Some(grapt_types::ModelVariantId::new("high").ok_or("effort")?);
        let selected = default_model(&catalog);
        let dto: grapt_api::u3a::ModelInfoDto = super::super::decode(selected.clone())?;
        assert_eq!(
            serde_json::to_value(dto)?["settings"]["metadata"]["default_effort"],
            "high"
        );
        assert!(selected.get("variant").is_none());
        assert!(accepts(&catalog, &selected));
        let rows = models(&catalog);
        let secondary = rows
            .as_array()
            .ok_or("rows")?
            .iter()
            .find(|row| row["providerID"] == "openrouter")
            .ok_or("secondary")?;
        assert!(
            secondary["settings"]["metadata"]["default_effort"].is_null(),
            "primary effort leaked across providers"
        );
        catalog.inventory = None;
        assert_eq!(
            default_model(&catalog)["settings"]["metadata"]["default_effort"],
            "high"
        );
        Ok(())
    }

    #[test]
    fn output_capacity_preserves_canonical_evidence_and_the_retained_dto(
    ) -> Result<(), Box<dyn std::error::Error>> {
        for (label, capacity) in [
            ("absent", None),
            ("zero", Some(0)),
            ("positive", Some(64000)),
        ] {
            let (scratch, mut catalog) = composed(&format!("output-{label}"));
            let mut public: Value = serde_json::from_str(CATALOG)?;
            match capacity {
                Some(capacity) => {
                    public["openai"]["models"]["gpt-5.6"]["limit"]["output"] = json!(capacity)
                }
                None => {
                    public["openai"]["models"] = json!({});
                }
            }
            std::fs::write(
                scratch.0.join("model-catalog.json"),
                serde_json::to_vec(&public)?,
            )?;
            let published = grapt_platform::PublishedConfig::load(&scratch.0)?;
            catalog.inventory = published.inventory();
            let inventory = catalog.inventory.as_ref().ok_or("missing inventory")?;
            let model = inventory
                .available()
                .iter()
                .find(|model| model.id().as_str() == "gpt-5.6")
                .ok_or("missing primary model")?;
            let before = model.metadata().clone();
            assert_eq!(
                before["limit"].get("output"),
                capacity.map(|value| json!(value)).as_ref()
            );
            // Context admission is independent of the retained output-limit spelling.
            assert_eq!(model.context(), Some(258400));
            let selected = default_model(&catalog);
            let dto: grapt_api::u3a::ModelInfoDto = super::super::decode(selected.clone())?;
            let decoded = serde_json::to_value(dto)?;
            assert_eq!(decoded["limit"]["output"], capacity.unwrap_or(0));
            assert!(accepts(&catalog, &selected));
            assert_eq!(
                model.metadata(),
                &before,
                "projection changed canonical evidence"
            );
        }
        Ok(())
    }

    #[test]
    fn fallback_visibility_never_promotes_unknown_rows_to_picker_or_selection(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (scratch, mut catalog) = composed("fallback-visibility");
        let mut public: Value = serde_json::from_str(CATALOG)?;
        public["openai"]["models"] = json!({});
        std::fs::write(
            scratch.0.join("model-catalog.json"),
            serde_json::to_vec(&public)?,
        )?;
        let mut rows = Vec::new();
        for (id, visibility) in [
            ("shown", Some(json!("list"))),
            ("direct", Some(json!("none"))),
            ("legacy", None),
            ("hidden", Some(json!("hide"))),
            ("future", Some(json!("experimental"))),
            ("null", Some(Value::Null)),
            ("numeric", Some(json!(7))),
            ("object", Some(json!({}))),
        ] {
            let mut row = json!({"slug":id,"context_window":1000,"supported_reasoning_levels":[]});
            if let Some(visibility) = visibility {
                row["visibility"] = visibility;
            }
            rows.push(row);
        }
        std::fs::write(
            scratch.0.join("account-models.json"),
            serde_json::to_vec(&json!({"models":rows}))?,
        )?;
        // Select an offered model before publishing the configuration.
        let mut config: Value = serde_json::from_str(CONFIG)?;
        for key in ["model", "base_model", "family"] {
            config["provider"][key] = json!("shown");
        }
        std::fs::write(scratch.0.join("grapt.jsonc"), serde_json::to_vec(&config)?)?;
        catalog.inventory = grapt_platform::PublishedConfig::load(&scratch.0)?.inventory();
        catalog.model = "shown".into();
        let projected = models(&catalog);
        assert_eq!(
            ids(&projected),
            vec!["shown", "direct", "legacy", "vendor-model-a"]
        );
        for row in projected.as_array().ok_or("missing picker rows")? {
            let _: grapt_api::u3a::ModelInfoDto = super::super::decode(row.clone())?;
            assert!(accepts(&catalog, row));
        }
        for id in ["hidden", "future", "null", "numeric", "object"] {
            assert!(
                !accepts(&catalog, &json!({"providerID":"openai","id":id})),
                "unoffered {id} became selectable"
            );
        }
        Ok(())
    }

    #[test]
    fn provider_status_labels_follow_each_account_and_keep_credentials_private(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_scratch, mut catalog) = composed("account-labels");
        catalog.accounts = [
            (
                grapt_types::ProviderId::new("openai").ok_or("provider")?,
                grapt_types::AccountId::new("account-a").ok_or("account")?,
            ),
            (
                grapt_types::ProviderId::new("openrouter").ok_or("provider")?,
                grapt_types::AccountId::new("account-b").ok_or("account")?,
            ),
        ]
        .into_iter()
        .collect();
        let rows = providers(&catalog);
        let rows = rows.as_array().ok_or("provider rows")?;
        assert_eq!(rows[0]["settings"]["account"], "account-a");
        assert_eq!(rows[1]["settings"]["account"], "account-b");
        assert!(!serde_json::to_string(rows)?.contains("sentinel"));
        assert!(!serde_json::to_string(rows)?.contains("credential"));
        catalog.accounts.clear();
        assert!(providers(&catalog)
            .as_array()
            .ok_or("provider rows")?
            .iter()
            .all(|row| row["settings"].get("account").is_none()));
        Ok(())
    }

    #[test]
    fn only_configured_providers_are_reported_connected() {
        let (_scratch, catalog) = composed("connected");
        assert_eq!(
            ids(&providers(&catalog)),
            vec!["openai".to_owned(), "openrouter".to_owned()],
            "the provider listing must be the connected set, not the public catalog"
        );
    }

    #[test]
    fn each_model_row_carries_the_provider_it_is_issuable_through() {
        let (_scratch, catalog) = composed("stamping");
        let rows = models(&catalog);
        let stamped: Vec<(String, String)> = rows
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|row| {
                Some((
                    row["providerID"].as_str()?.to_owned(),
                    row["id"].as_str()?.to_owned(),
                ))
            })
            .collect();
        assert_eq!(
            stamped,
            vec![
                ("openai".to_owned(), "gpt-5.6".to_owned()),
                ("openrouter".to_owned(), "vendor-model-a".to_owned()),
            ],
            "each model row must carry its own provider, not the default connection"
        );
        // Every stamped provider is one the frontend was told is connected, so
        // no row is silently dropped by the renderer.
        let connected = ids(&providers(&catalog));
        assert!(
            stamped
                .iter()
                .all(|(provider, _)| connected.contains(provider)),
            "a model row named a provider absent from the connected listing"
        );
    }

    #[test]
    fn selection_is_accepted_only_on_a_whole_connected_coordinate() {
        let (_scratch, catalog) = composed("accepts");
        assert!(accepts(
            &catalog,
            &json!({"providerID":"openrouter","id":"vendor-model-a"})
        ));
        assert!(accepts(
            &catalog,
            &json!({"providerID":"openai","id":"gpt-5.6"})
        ));
        // A model id that exists, named under the wrong connected provider.
        assert!(
            !accepts(
                &catalog,
                &json!({"providerID":"openai","id":"vendor-model-a"})
            ),
            "a model was accepted under a provider that cannot issue it"
        );
        // A provider that exists in the public catalog but has no account.
        assert!(
            !accepts(&catalog, &json!({"providerID":"unconfigured","id":"never"})),
            "an unconfigured provider was accepted as a selection"
        );
        // The default model resolves on the whole coordinate too.
        assert_eq!(
            default_model(&catalog)["providerID"].as_str(),
            Some("openai")
        );
    }
}
