//! Compose connection-owned listings with catalog-owned model metadata.
use super::*;

impl ModelInventory {
    pub(super) fn compose_with_aliases(
        public: Value,
        mut accounts: Vec<(ProviderId, Value, Metadata, DiscoveryShape)>,
        window: Option<u64>,
        aliases: &BTreeMap<ProviderId, CatalogAlias>,
    ) -> Result<Self, String> {
        check_account_count(accounts.len())?;
        use grapt_types::ClockAuthority;
        let now = grapt_store::SystemClock.now().as_millis();
        for (provider, listing, metadata, shape) in &mut accounts {
            let catalog = aliases
                .get(provider)
                .map_or(&*provider, |alias| &alias.provider);
            if catalog.as_str() == "openai"
                && *metadata == Metadata::CatalogOrListing
                && *shape == DiscoveryShape::Codex
            {
                chatgpt_window::prepare(listing, now, window)?;
            }
        }
        let providers = public
            .as_object()
            .ok_or("model catalog must be an object")?;
        if providers.is_empty() || providers.len() > MAX_PROVIDERS {
            return Err("model catalog provider count is outside its bound".into());
        }
        let mut count = 0usize;
        for (id, row) in providers {
            if row.get("id").and_then(Value::as_str) != Some(id.as_str()) {
                return Err(format!("catalog provider identity disagrees with key {id}"));
            }
            let models = row
                .get("models")
                .and_then(Value::as_object)
                .ok_or_else(|| format!("provider {id} has no model object"))?;
            count = count.saturating_add(models.len());
            if count > MAX_MODELS {
                return Err("model catalog exceeds its model bound".into());
            }
        }
        let mut connected: Vec<ProviderId> = Vec::with_capacity(accounts.len());
        let mut available = Vec::new();
        for (provider, account, metadata, shape) in &accounts {
            if connected.contains(provider) {
                return Err(format!(
                    "two configured accounts both claim provider {}",
                    provider.as_str()
                ));
            }
            connected.push(provider.clone());
            let catalog = aliases
                .get(provider)
                .map_or(provider, |alias| &alias.provider);
            let models = providers
                .get(catalog.as_str())
                .and_then(|row| row.get("models"))
                .and_then(Value::as_object)
                .ok_or("connected provider is absent from catalog")?;
            let first = available.len();
            match shape {
                DiscoveryShape::Codex => {
                    Self::admit_account(catalog, models, account, *metadata, &mut available)?
                }
                DiscoveryShape::ProviderListOrLegacyCodex
                    if account.get("data").is_none()
                        && (account.is_array() || account.get("models").is_some()) =>
                {
                    Self::admit_account(catalog, models, account, *metadata, &mut available)?
                }
                DiscoveryShape::ProviderList | DiscoveryShape::ProviderListOrLegacyCodex => {
                    Self::admit_listing(catalog, models, account, &mut available)?
                }
            }
            // Catalog identity determines metadata and provider-specific model
            // capabilities. Issuance and session selection always use the
            // connection that supplied this listing.
            for row in &mut available[first..] {
                row.provider = provider.clone();
            }
        }
        // Build display aliases only after all metadata lookups, so a
        // connection named after a different provider cannot alter its catalog.
        let mut projected: BTreeMap<String, Value> = providers.clone().into_iter().collect();
        for (connection, alias) in aliases {
            let mut row = providers
                .get(alias.provider.as_str())
                .ok_or("connection catalog provider is absent")?
                .clone();
            row["id"] = connection.as_str().into();
            if let Some(label) = &alias.label {
                if label.is_empty() || label.len() > 256 || label.chars().any(char::is_control) {
                    return Err("connection label is invalid".into());
                }
                row["name"] = label.as_str().into();
            }
            projected.insert(connection.as_str().to_owned(), row);
        }
        Ok(Self {
            providers: projected,
            connected,
            available,
        })
    }
}
