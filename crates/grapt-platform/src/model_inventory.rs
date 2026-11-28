//! Bounded catalog data, separate from credential and target authority.
//!
//! The public models.dev snapshot describes providers. The connected account's
//! discovery snapshot determines which of its models may actually be issued.
//! Reading metadata never creates a connection to another provider.
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

use grapt_types::{
    BaseModelId, ModelCatalogRecord, ModelFamilyId, ModelId, ModelVariantId, ProviderId,
};
use serde_json::Value;

mod compose;
mod reasoning;

/// The reasoning variants a listing row offers for one model of a catalog
/// provider: what a run of that model may select.
pub fn offered_variants(
    provider: &str,
    model: &str,
    row: &Value,
) -> Result<Vec<grapt_types::ModelVariantId>, String> {
    reasoning::offered(provider, model, row)
}
mod thinking;
pub use thinking::AnthropicThinkingMode;
mod chatgpt_window;
#[cfg(test)]
mod chatgpt_window_tests;
mod image_capabilities;
mod provider_listing;

const MAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PROVIDERS: usize = 256;
const MAX_MODELS: usize = 16_384;
/// Discovery breadth for one account.
///
/// 256 was sized when the only connected account was a single ChatGPT
/// subscription advertising nine models. A real aggregator account is a
/// different scale: measured on 2026-09-06, a live OpenRouter account
/// advertised 430 models, of which 359 also carry public catalog metadata. A
/// bound that cannot represent a real configured account is not a safety
/// property, it is a defect, so this admits that class while the total
/// issuable-record bound below still holds the product of models and variants.
const MAX_ACCOUNT_MODELS: usize = 1_024;
const MAX_VARIANTS: usize = 32;
/// Configured accounts admitted into one inventory. Discovery breadth is
/// bounded per account above; this bounds how many accounts may compose.
const MAX_ACCOUNTS: usize = 8;
/// Ceiling on the issuable rows every account contributes together. The
/// per-account bounds alone would admit a product large enough to matter, and
/// an unbounded catalog is exactly what the platform refuses elsewhere.
const MAX_ISSUABLE_RECORDS: usize = 4_096;

/// Input modalities one listed model may state.
const MAX_MODALITIES: usize = 8;

/// The shape of one account's discovery snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiscoveryShape {
    /// The ChatGPT/Codex listing: `models[]` rows carrying `slug`,
    /// `visibility`, `context_window` and reasoning levels. Every model it
    /// lists must be described by the catalog.
    Codex,
    /// A provider API's `GET /v1/models` (`data[].id`). The catalog can supply
    /// metadata; a listing's `max_model_len` can state a tighter context limit.
    /// Undescribed models remain available with explicit unknown metadata.
    /// A paginated listing (`has_more`) refuses.
    ProviderList,
    /// Secondary accounts historically stored the provider's account snapshot shape
    /// even for API providers. Accept that validated legacy shape only when no
    /// data array is present; malformed provider listings never fall back.
    ProviderListOrLegacyCodex,
}

/// One configured account's discovery input: which provider it connects and
/// which bounded discovery snapshot names its issuable models.
#[derive(Debug, Clone)]
pub(crate) struct InventoryAccount {
    pub(crate) provider: ProviderId,
    pub(crate) catalog_provider: ProviderId,
    pub(crate) label: Option<String>,
    pub(crate) discovery: String,
    pub(crate) metadata: Metadata,
    pub(crate) shape: DiscoveryShape,
}

struct CatalogAlias {
    provider: ProviderId,
    label: Option<String>,
}

/// Where an account's listed models take their metadata from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Metadata {
    /// The public catalog only: a listed model it does not describe refuses.
    Catalog,
    /// The public catalog, else the listing row itself. Only the ChatGPT
    /// OAuth connection: its listing is the Codex backend's own `/models`,
    /// which describes every model it offers, and the public catalog lags
    /// new ones (FIRST-RUN-MODEL-MATCH).
    CatalogOrListing,
}

#[derive(Debug, Clone)]
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
pub struct ConnectedModel {
    provider: ProviderId,
    id: ModelId,
    metadata: Value,
    context: Option<u64>,
    variants: Vec<ModelVariantId>,
    default_variant: Option<ModelVariantId>,
    responses_lite: bool,
    summary_parameter: bool,
    effort_updates: bool,
    thinking: Option<AnthropicThinkingMode>,
    original_image_detail: bool,
    image_input: bool,
}

impl ConnectedModel {
    /// The provider this model is issuable through. Carried per row rather
    /// than read from an ambient "the connected provider" field, so a second
    /// configured account can never inherit the first one's identity.
    pub fn provider(&self) -> &ProviderId {
        &self.provider
    }
    pub fn id(&self) -> &ModelId {
        &self.id
    }
    pub fn metadata(&self) -> &Value {
        &self.metadata
    }
    pub fn context(&self) -> Option<u64> {
        self.context
    }
    /// The account catalog's `use_responses_lite`; absent means false.
    pub fn responses_lite(&self) -> bool {
        self.responses_lite
    }
    /// Whether the route accepts `reasoning.summary`: the account catalog's
    /// `supports_reasoning_summary_parameter` (the field Codex gates it on),
    /// or `supports_reasoning_summaries` where a row does not state it.
    pub fn summary_parameter(&self) -> bool {
        self.summary_parameter
    }
    /// Thinking mode stated by discovery, or the documented native Claude fallback.
    /// Unknown models retain the provider's default instead of guessing support.
    pub fn anthropic_thinking(&self) -> Option<AnthropicThinkingMode> {
        self.thinking
    }
    /// Whether the effective account/catalog metadata advertises image input.
    pub fn supports_images(&self) -> bool {
        self.image_input
    }
    /// The account's explicit original-detail capability; unknown is false.
    pub fn original_image_detail(&self) -> bool {
        self.original_image_detail
    }
    /// The connected account explicitly advertises chronological Responses updates.
    /// Missing capability data keeps ordinary request-level effort.
    pub fn effort_updates(&self) -> bool {
        self.effort_updates
    }
    pub fn variants(&self) -> &[ModelVariantId] {
        &self.variants
    }
    /// The advertised or documented default, only when supported by this row.
    /// An unknown default remains absent and leaves the choice to the backend.
    pub fn default_variant(&self) -> Option<&ModelVariantId> {
        self.default_variant.as_ref()
    }
}

#[derive(Debug, Clone)]
pub struct ModelInventory {
    providers: BTreeMap<String, Value>,
    connected: Vec<ProviderId>,
    available: Vec<ConnectedModel>,
}

impl ModelInventory {
    /// Validate the ChatGPT first-run snapshots in memory, using exactly the
    /// same catalog, discovery and record rules as profile publication. This
    /// creates no profile, credential, store or provider connection.
    pub fn chatgpt_listing(public: Value, listing: Value) -> Result<Self, String> {
        Self::first_run(
            public,
            listing,
            "openai",
            "models.dev-chatgpt",
            "chatgpt-codex",
        )
    }

    /// Validate a first-run composition without creating credentials or a profile.
    /// The same composed-pair table selects discovery shape at publication.
    pub fn first_run(
        public: Value,
        listing: Value,
        provider: &str,
        source: &str,
        family: &str,
    ) -> Result<Self, String> {
        let pair = crate::COMPOSED_CATALOGS
            .iter()
            .find(|pair| {
                pair.provider == provider && pair.source == source && pair.family == family
            })
            .ok_or("first-run catalog composition is not supported")?;
        let provider = ProviderId::new(provider).ok_or("first-run provider identity is invalid")?;
        let metadata = match pair.shape {
            DiscoveryShape::Codex => Metadata::CatalogOrListing,
            DiscoveryShape::ProviderList | DiscoveryShape::ProviderListOrLegacyCodex => {
                Metadata::Catalog
            }
        };
        let inventory = Self::compose(public, vec![(provider, listing, metadata, pair.shape)])?;
        inventory.records()?;
        Ok(inventory)
    }

    /// Explicit startup input. An absent, unreadable or malformed configured
    /// snapshot refuses startup; none of those states becomes an empty catalog.
    ///
    /// Every configured account names its own bounded discovery snapshot. The
    /// public catalog is read once and shared: it describes providers, and
    /// reading it still creates no connection to any of them.
    pub(crate) fn load(
        root: &Path,
        accounts: &[InventoryAccount],
        window: Option<u64>,
    ) -> Result<Self, String> {
        check_account_count(accounts.len())?;
        let public = read(root, "model-catalog.json")?;
        Self::load_with_public(root, public, accounts, window)
    }

    fn load_with_public(
        root: &Path,
        public: Value,
        accounts: &[InventoryAccount],
        window: Option<u64>,
    ) -> Result<Self, String> {
        check_account_count(accounts.len())?;
        let mut discovered = Vec::with_capacity(accounts.len());
        for account in accounts {
            discovered.push((
                account.provider.clone(),
                read(root, discovery_name(&account.discovery)?)?,
                account.metadata,
                account.shape,
            ));
        }
        let aliases = accounts
            .iter()
            .map(|account| {
                (
                    account.provider.clone(),
                    CatalogAlias {
                        provider: account.catalog_provider.clone(),
                        label: account.label.clone(),
                    },
                )
            })
            .collect();
        Self::compose_with_aliases(public, discovered, window, &aliases)
    }

    #[cfg(test)]
    fn decode(public: Value, accounts: Vec<(ProviderId, Value)>) -> Result<Self, String> {
        Self::compose(
            public,
            accounts
                .into_iter()
                .map(|(provider, account)| {
                    (provider, account, Metadata::Catalog, DiscoveryShape::Codex)
                })
                .collect(),
        )
    }

    fn compose(
        public: Value,
        accounts: Vec<(ProviderId, Value, Metadata, DiscoveryShape)>,
    ) -> Result<Self, String> {
        Self::compose_with_window(public, accounts, None)
    }

    fn compose_with_window(
        public: Value,
        accounts: Vec<(ProviderId, Value, Metadata, DiscoveryShape)>,
        window: Option<u64>,
    ) -> Result<Self, String> {
        Self::compose_with_aliases(public, accounts, window, &BTreeMap::new())
    }

    /// Decode one account's discovery snapshot against that account's own
    /// provider metadata. Isolation is structural: this never sees another
    /// account's rows, so one account's discovery cannot widen another's.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn admit_account(
        provider: &ProviderId,
        models: &serde_json::Map<String, Value>,
        account: &Value,
        source: Metadata,
        available: &mut Vec<ConnectedModel>,
    ) -> Result<(), String> {
        let discovered = account
            .get("models")
            .and_then(Value::as_array)
            .or_else(|| account.as_array())
            .ok_or("account discovery must contain a model array")?;
        if discovered.len() > MAX_ACCOUNT_MODELS {
            return Err("account model count exceeds its bound".into());
        }
        let mut seen = BTreeSet::new();
        let mut admitted = 0usize;
        for row in discovered {
            let raw = row
                .get("slug")
                .or_else(|| row.get("id"))
                .and_then(Value::as_str)
                .ok_or("account model has no identity")?;
            if !seen.insert(raw) {
                return Err(format!("account discovery repeats model {raw}"));
            }
            let offered = match row.get("visibility") {
                None => true,
                Some(Value::String(value)) => matches!(value.as_str(), "list" | "none"),
                Some(_) => false,
            };
            if row.get("visibility").and_then(Value::as_str) == Some("hide")
                || (source == Metadata::CatalogOrListing && !offered)
            {
                continue;
            }
            let metadata = match (models.get(raw), source) {
                (Some(metadata), _) => metadata.clone(),
                (None, Metadata::CatalogOrListing) => listing_metadata(raw, row)?,
                (None, Metadata::Catalog) => {
                    return Err(format!("account model {raw} has no catalog metadata"));
                }
            };
            if metadata.get("id").and_then(Value::as_str) != Some(raw) {
                return Err(format!("model catalog identity disagrees with key {raw}"));
            }
            let context = row
                .get("context_window")
                .and_then(Value::as_u64)
                .filter(|value| *value > 0)
                .ok_or("account model has no positive context window")?;
            let percent = match row.get("effective_context_window_percent") {
                None => 95,
                Some(value) => value
                    .as_u64()
                    .filter(|value| (1..=100).contains(value))
                    .ok_or("account context percentage is invalid")?,
            };
            let context = context
                .checked_mul(percent)
                .map(|value| value / 100)
                .filter(|value| *value > 0)
                .ok_or("account context window overflows")?;
            let efforts = row
                .get("supported_reasoning_levels")
                .and_then(Value::as_array)
                .ok_or("account model has no reasoning-level array")?;
            if efforts.len() > MAX_VARIANTS {
                return Err("model reasoning levels exceed their bound".into());
            }
            let mut variants = Vec::new();
            let mut seen_efforts = BTreeSet::new();
            for effort in efforts {
                let raw = effort
                    .get("effort")
                    .and_then(Value::as_str)
                    .ok_or("reasoning level has no effort")?;
                if !seen_efforts.insert(raw) {
                    return Err("account reasoning level is repeated".into());
                }
                variants
                    .push(ModelVariantId::new(raw).ok_or("reasoning effort identity is invalid")?);
            }
            let default_variant = reasoning::default_for(provider.as_str(), raw, row, &variants)?;
            let flag = |key: &str| match row.get(key) {
                None => Ok(false),
                Some(value) => value
                    .as_bool()
                    .ok_or_else(|| format!("account model {key} is not a boolean")),
            };
            let image_input =
                image_capabilities::supports_images(&metadata, row.get("input_modalities"))?;
            let original_image_detail = flag("supports_image_detail_original")?;
            let responses_lite = flag("use_responses_lite")?;
            // Codex gates reasoning.summary on the parameter flag; a row
            // without it keeps the summaries flag's answer.
            let summary_parameter = match row.get("supports_reasoning_summary_parameter") {
                None => flag("supports_reasoning_summaries")?,
                Some(_) => flag("supports_reasoning_summary_parameter")?,
            };
            if available.len() >= MAX_ISSUABLE_RECORDS {
                return Err("configured accounts exceed the issuable model bound".into());
            }
            available.push(ConnectedModel {
                provider: provider.clone(),
                id: ModelId::new(raw).ok_or("account model identity is invalid")?,
                metadata,
                context: Some(context),
                variants,
                default_variant,
                responses_lite,
                summary_parameter,
                effort_updates: flag("supports_reasoning_effort_updates")?,
                thinking: if provider.as_str() == "anthropic" {
                    thinking::mode(raw, row)?
                } else {
                    None
                },
                original_image_detail,
                image_input,
            });
            admitted = admitted.saturating_add(1);
        }
        if admitted == 0 {
            return Err("connected account has no available catalog models".into());
        }
        Ok(())
    }

    pub fn providers(&self) -> &BTreeMap<String, Value> {
        &self.providers
    }

    /// The configured, credentialed providers, in configuration order. This is
    /// the connected set; the public catalog map above is metadata breadth and
    /// says nothing about whether a provider can be issued.
    pub fn connected(&self) -> &[ProviderId] {
        &self.connected
    }
    pub fn available(&self) -> &[ConnectedModel] {
        &self.available
    }

    pub(crate) fn records(&self) -> Result<Vec<ModelCatalogRecord>, String> {
        let count = self.available.iter().try_fold(0usize, |count, model| {
            count
                .checked_add(1 + model.variants.len())
                .filter(|count| *count <= MAX_ISSUABLE_RECORDS)
                .ok_or("configured accounts exceed the issuable record bound")
        })?;
        let mut records = Vec::with_capacity(count);
        for model in &self.available {
            let base =
                BaseModelId::new(model.id.as_str()).ok_or("model base identity is invalid")?;
            // An absent family is isolated to this base model, never inferred
            // from a provider name to permit a cross-model agent-default hop.
            let family = ModelFamilyId::new(
                model
                    .metadata
                    .get("family")
                    .and_then(Value::as_str)
                    .unwrap_or(model.id.as_str()),
            )
            .ok_or("model family identity is invalid")?;
            for variant in std::iter::once(None).chain(model.variants.iter().cloned().map(Some)) {
                records.push(ModelCatalogRecord::new(
                    model.provider.clone(),
                    model.id.clone(),
                    base.clone(),
                    family.clone(),
                    variant,
                ));
            }
        }
        Ok(records)
    }
}

fn check_account_count(count: usize) -> Result<(), String> {
    if count == 0 {
        return Err("an inventory needs at least one configured account".into());
    }
    if count > MAX_ACCOUNTS {
        return Err("configured account count exceeds its bound".into());
    }
    Ok(())
}

/// Metadata for a listed model the public catalog does not describe, from
/// what the account's listing row itself states: its display name, whether
/// it drives a shell tool, and its input modalities. The row states no output
/// limit, which stays absent rather than becoming a measured zero. The context
/// window is read from the row for every model, described or not.
fn listing_metadata(raw: &str, row: &Value) -> Result<Value, String> {
    let name = match row.get("display_name") {
        None => raw,
        Some(value) => value
            .as_str()
            .filter(|name| !name.is_empty() && name.len() <= 256)
            .ok_or_else(|| format!("account model {raw} has an invalid display name"))?,
    };
    let input = match row.get("input_modalities") {
        None => vec!["text"],
        Some(value) => value
            .as_array()
            .filter(|modalities| !modalities.is_empty() && modalities.len() <= MAX_MODALITIES)
            .and_then(|modalities| modalities.iter().map(Value::as_str).collect())
            .ok_or_else(|| format!("account model {raw} has invalid input modalities"))?,
    };
    Ok(serde_json::json!({
        "id": raw,
        "name": name,
        "tool_call": matches!(row.get("shell_type").and_then(Value::as_str),
            Some("unified_exec" | "default" | "local" | "shell_command")),
        "modalities": {"input": input, "output": ["text"]},
        "limit": {},
        "metadata_source": "account-listing",
    }))
}

/// Admit a configured discovery snapshot name as a PLAIN file name inside the
/// service root. A separator, a drive prefix, a parent hop or a non-JSON suffix
/// is refused here rather than reaching the filesystem: configuration names an
/// input, it never widens where the platform is allowed to read.
fn discovery_name(name: &str) -> Result<&str, String> {
    if name.is_empty() || name.len() > 64 {
        return Err("account discovery file name is outside its bound".into());
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(format!(
            "account discovery file name {name:?} is not a plain name"
        ));
    }
    if name.starts_with('.') || name.contains("..") || !name.ends_with(".json") {
        return Err(format!(
            "account discovery file name {name:?} is not a plain name"
        ));
    }
    Ok(name)
}

fn read(root: &Path, name: &str) -> Result<Value, String> {
    let path = root.join(name);
    let file =
        std::fs::File::open(&path).map_err(|error| format!("{name} is unreadable: {error}"))?;
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err(format!("{name} is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(format!("{name} exceeds its 16 MiB bound"));
    }
    serde_json::from_slice(&bytes).map_err(|error| format!("{name} is malformed: {error}"))
}

#[cfg(test)]
#[path = "model_inventory_bounds_tests.rs"]
mod bounds_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn inputs() -> (Value, Value) {
        (
            json!({
                "openai":{"id":"openai","name":"OpenAI","models":{
                    "first":{"id":"first","family":"first"},"second":{"id":"second","family":"second"}
                }},
                "openrouter":{"id":"openrouter","name":"OpenRouter","models":{"other":{"id":"other"}}}
            }),
            json!({"models":[
                {"slug":"first","visibility":"list","context_window":272000,"effective_context_window_percent":95,
                 "supported_reasoning_levels":[{"effort":"low"},{"effort":"high"}]},
                {"slug":"second","visibility":"list","context_window":128000,"supported_reasoning_levels":[]},
                {"slug":"private","visibility":"hide"}
            ]}),
        )
    }

    #[test]
    fn discovery_keeps_provider_breadth_but_only_issues_connected_account_rows(
    ) -> Result<(), String> {
        let (public, account) = inputs();
        let provider = ProviderId::new("openai").ok_or("provider")?;
        let inventory = ModelInventory::decode(public, vec![(provider.clone(), account)])?;
        assert_eq!(inventory.providers.len(), 2);
        assert!(inventory.providers.contains_key("openrouter"));
        // Metadata breadth is retained, but openrouter is NOT connected: it has
        // no configured account, so it can never be issued from this inventory.
        assert_eq!(inventory.connected(), std::slice::from_ref(&provider));
        assert_eq!(inventory.available.len(), 2);
        assert_eq!(inventory.available[0].context, Some(258400));
        assert_eq!(inventory.available[1].context, Some(121600));
        let records = inventory.records()?;
        assert_eq!(records.len(), 4);
        assert!(records.iter().all(|record| record.provider() == &provider));
        assert!(records
            .iter()
            .all(|record| record.model().as_str() != "private"));
        assert!(records
            .iter()
            .any(|record| record.variant().is_some_and(|v| v.as_str() == "high")));
        Ok(())
    }

    /// `use_responses_lite` is read per account row: true where the account
    /// says so, false where it is absent, and refused when it is not a boolean.
    #[test]
    fn responses_lite_is_read_from_the_account_row() -> Result<(), String> {
        let provider = ProviderId::new("openai").ok_or("provider")?;
        let (public, mut account) = inputs();
        account["models"][0]["use_responses_lite"] = json!(true);
        let inventory = ModelInventory::decode(public, vec![(provider.clone(), account)])?;
        assert!(inventory.available[0].responses_lite());
        assert!(!inventory.available[1].responses_lite());
        let (public, mut account) = inputs();
        account["models"][1]["supports_reasoning_summaries"] = json!(true);
        account["models"][0]["supports_reasoning_summaries"] = json!(true);
        account["models"][0]["supports_reasoning_summary_parameter"] = json!(false);
        let inventory = ModelInventory::decode(public, vec![(provider.clone(), account)])?;
        assert!(
            !inventory.available[0].summary_parameter(),
            "an explicit parameter false wins over supporting summaries"
        );
        assert!(
            inventory.available[1].summary_parameter(),
            "a row without the parameter flag keeps the summaries answer"
        );
        let (public, mut account) = inputs();
        account["models"][0]["supports_reasoning_summary_parameter"] = json!(1);
        assert!(ModelInventory::decode(public, vec![(provider.clone(), account)]).is_err());
        let (public, mut account) = inputs();
        account["models"][0]["use_responses_lite"] = json!("yes");
        assert!(ModelInventory::decode(public, vec![(provider, account)]).is_err());
        Ok(())
    }

    #[test]
    fn image_capabilities_preserve_explicit_account_values() -> Result<(), String> {
        let provider = ProviderId::new("openai").ok_or("provider")?;
        let (mut public, mut account) = inputs();
        public["openai"]["models"]["first"]["modalities"] = json!({"input":["text","image"]});
        account["models"][0]["supports_image_detail_original"] = json!(true);
        account["models"][1]["supports_image_detail_original"] = json!(false);
        account["models"][1]["input_modalities"] = json!(["text", "image"]);
        let inventory = ModelInventory::decode(public, vec![(provider.clone(), account)])?;
        assert!(inventory.available[0].supports_images());
        assert!(inventory.available[0].original_image_detail());
        assert!(inventory.available[1].supports_images());
        assert!(!inventory.available[1].original_image_detail());
        let (public, account) = inputs();
        let inventory = ModelInventory::decode(public, vec![(provider.clone(), account)])?;
        assert!(!inventory.available[0].original_image_detail());
        for value in [Value::Null, json!(1), json!("true")] {
            let (public, mut account) = inputs();
            account["models"][0]["supports_image_detail_original"] = value;
            let error = ModelInventory::decode(public, vec![(provider.clone(), account)])
                .err()
                .ok_or("malformed image capability was accepted")?;
            assert!(error.contains("supports_image_detail_original"), "{error}");
        }
        Ok(())
    }

    #[test]
    fn inconsistent_or_partial_discovery_refuses_instead_of_fabricating_limits(
    ) -> Result<(), String> {
        let provider = ProviderId::new("openai").ok_or("provider")?;
        for mutation in 0..4 {
            let (mut public, mut account) = inputs();
            match mutation {
                0 => account["models"][0]["context_window"] = Value::Null,
                1 => account["models"][0]["effective_context_window_percent"] = json!(101),
                2 => public["openai"]["models"]["first"]["id"] = json!("impostor"),
                _ => account["models"][1]["slug"] = json!("first"),
            }
            assert!(
                ModelInventory::decode(public, vec![(provider.clone(), account)]).is_err(),
                "mutation {mutation} survived"
            );
        }
        Ok(())
    }

    /// The J01 positive witness for a second configured account: each account
    /// contributes only its OWN provider's rows, and every issuable record
    /// carries the provider it is actually reachable through.
    #[test]
    fn a_second_configured_account_issues_its_own_provider_rows() -> Result<(), String> {
        let (public, account) = inputs();
        let openai = ProviderId::new("openai").ok_or("provider")?;
        let openrouter = ProviderId::new("openrouter").ok_or("provider")?;
        let other = json!({"models":[
            {"slug":"other","visibility":"list","context_window":100000,
             "supported_reasoning_levels":[]}
        ]});
        let inventory = ModelInventory::decode(
            public,
            vec![(openai.clone(), account), (openrouter.clone(), other)],
        )?;
        assert_eq!(inventory.connected(), &[openai.clone(), openrouter.clone()]);
        assert_eq!(inventory.available.len(), 3);
        let records = inventory.records()?;
        assert_eq!(records.len(), 5);
        let routed: Vec<_> = records
            .iter()
            .filter(|record| record.provider() == &openrouter)
            .collect();
        assert_eq!(routed.len(), 1);
        assert_eq!(routed[0].model().as_str(), "other");
        // The openai account's models did not acquire the second provider.
        assert!(records
            .iter()
            .filter(|record| record.provider() == &openai)
            .all(|record| record.model().as_str() != "other"));
        Ok(())
    }

    /// Account isolation failure counterexample: one account may not claim a
    /// provider another already connected, and an account may not issue a model
    /// its own provider's catalog does not describe.
    #[test]
    fn account_composition_refuses_duplicate_and_foreign_claims() -> Result<(), String> {
        let openai = ProviderId::new("openai").ok_or("provider")?;
        let openrouter = ProviderId::new("openrouter").ok_or("provider")?;
        let (public, account) = inputs();
        assert!(
            ModelInventory::decode(
                public.clone(),
                vec![
                    (openai.clone(), account.clone()),
                    (openai.clone(), account.clone()),
                ],
            )
            .is_err(),
            "two accounts claiming one provider survived"
        );
        // "first" exists in the openai catalog, never in openrouter's.
        assert!(
            ModelInventory::decode(public, vec![(openrouter, account)]).is_err(),
            "an account issuing a foreign provider's model survived"
        );
        Ok(())
    }

    /// FIRST-RUN-MODEL-MATCH: the ChatGPT primary's listing describes a model
    /// the public catalog does not yet; a catalog-only account still refuses.
    #[test]
    fn the_chatgpt_primary_reads_an_undescribed_model_from_its_listing() -> Result<(), String> {
        let openai = ProviderId::new("openai").ok_or("provider")?;
        let (public, mut account) = inputs();
        account["models"].as_array_mut().ok_or("models")?.push(
            json!({"slug":"gpt-new","display_name":"GPT-New","visibility":"list",
                "shell_type":"unified_exec","input_modalities":["text","image"],
                "context_window":400000,"supported_reasoning_levels":[{"effort":"medium"}]}),
        );
        let inventory = ModelInventory::compose(
            public.clone(),
            vec![(
                openai.clone(),
                account.clone(),
                Metadata::CatalogOrListing,
                DiscoveryShape::Codex,
            )],
        )?;
        assert_eq!(inventory.available.len(), 3);
        let new = &inventory.available[2];
        assert_eq!(new.id().as_str(), "gpt-new");
        assert_eq!(new.context(), Some(380000));
        assert_eq!(
            new.metadata(),
            &json!({"id":"gpt-new","name":"GPT-New","tool_call":true,
                "modalities":{"input":["text","image"],"output":["text"]},
                "limit":{},"metadata_source":"account-listing"})
        );
        // A described model keeps its public catalog metadata. The undescribed
        // one states no family, so its records isolate it to its own id.
        assert_eq!(inventory.available[0].metadata()["family"], "first");
        assert_eq!(inventory.records()?.len(), 6);
        let mut disabled = account.clone();
        disabled["models"][3]["shell_type"] = json!("disabled");
        disabled["models"][3]["context_window"] = json!(400000);
        disabled["models"][3]["max_context_window"] = json!(400000);
        let disabled = ModelInventory::compose(
            public.clone(),
            vec![(
                openai.clone(),
                disabled,
                Metadata::CatalogOrListing,
                DiscoveryShape::Codex,
            )],
        )?;
        assert_eq!(disabled.available[2].metadata()["tool_call"], false);
        assert_eq!(disabled.available[2].context(), Some(380000));
        assert_eq!(
            ModelInventory::compose(
                public.clone(),
                vec![(
                    openai.clone(),
                    account.clone(),
                    Metadata::Catalog,
                    DiscoveryShape::Codex
                )]
            )
            .err(),
            Some("account model gpt-new has no catalog metadata".to_owned())
        );
        for (key, bad) in [
            ("display_name", json!("")),
            ("input_modalities", json!([])),
            ("input_modalities", json!([1])),
        ] {
            let mut account = account.clone();
            account["models"][3][key] = bad;
            assert!(
                ModelInventory::compose(
                    public.clone(),
                    vec![(
                        openai.clone(),
                        account,
                        Metadata::CatalogOrListing,
                        DiscoveryShape::Codex
                    )]
                )
                .is_err(),
                "an invalid {key} survived"
            );
        }
        Ok(())
    }

    #[test]
    fn a_configured_discovery_name_cannot_widen_where_the_platform_reads() {
        for name in [
            "../secret.json",
            "sub/dir.json",
            "C:/absolute.json",
            ".hidden.json",
            "account-models.txt",
            "",
        ] {
            assert!(
                discovery_name(name).is_err(),
                "discovery name {name:?} was admitted"
            );
        }
        assert_eq!(
            discovery_name("account-models.json"),
            Ok("account-models.json")
        );
        assert_eq!(
            discovery_name("openrouter-account-models.json"),
            Ok("openrouter-account-models.json")
        );
    }
}
