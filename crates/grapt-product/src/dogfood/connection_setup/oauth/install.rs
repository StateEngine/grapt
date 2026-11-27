//! ChatGPT publishes through the shared vault, approval and live generation.
use super::*;
use grapt_connections::store::{
    next_key_files, AuthMethod, Connection, CredentialFormat, DefaultModel, Family,
};
use grapt_login::vault::{ApiDestination, DestinationBinding, OAuthRefreshDestination};
use grapt_types::{AccountId, CredentialVersion, ModelId, ModelRouteId, PolicyVersion, ProviderId};

fn listing(bytes: &[u8], now: i64) -> Result<(Vec<u8>, String), String> {
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("ChatGPT model list exceeds its bound".into());
    }
    let mut listing: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "invalid ChatGPT model list")?;
    let rows = listing["models"]
        .as_array_mut()
        .filter(|rows| rows.len() <= 1024)
        .ok_or("invalid ChatGPT model list")?;
    rows.retain(|row| match row.get("visibility") {
        None => true,
        Some(serde_json::Value::String(visibility)) => {
            matches!(visibility.as_str(), "list" | "none")
        }
        _ => false,
    });
    let provider = KeyProvider::from_id("openai")?.ok_or("OpenAI catalog metadata is absent")?;
    let keys: Vec<_> = rows
        .iter()
        .filter_map(|row| row["slug"].as_str().map(|id| json!({"id":id})))
        .collect();
    let known = provider.supported_listing(
        &serde_json::to_vec(&json!({"data":keys})).map_err(|_| "invalid ChatGPT model list")?,
    )?;
    let model = provider.default_model(&known)?;
    let known: serde_json::Value =
        serde_json::from_slice(&known).map_err(|_| "invalid ChatGPT model list")?;
    let known = known["data"]
        .as_array()
        .ok_or("invalid ChatGPT model list")?;
    rows.retain(|row| {
        row["slug"]
            .as_str()
            .is_some_and(|slug| known.iter().any(|model| model["id"].as_str() == Some(slug)))
    });
    listing
        .as_object_mut()
        .ok_or("invalid ChatGPT model list")?
        .insert("secode_fetched_at_ms".into(), now.into());
    let bytes = serde_json::to_vec(&listing).map_err(|_| "invalid ChatGPT model list")?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("ChatGPT model list exceeds its bound".into());
    }
    Ok((bytes, model))
}

impl ConnectionSetup {
    #[allow(clippy::too_many_arguments)]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) fn install_oauth(
        &self,
        before: Option<Connections>,
        target: Option<DtoString>,
        label: String,
        issuer: &str,
        backend: &str,
        tokens: chatgpt::Tokens,
        raw_listing: &[u8],
    ) -> Result<(), String> {
        let _protection = grapt_login::profile_security::ProtectionBatch::begin()
            .map_err(|_| "profile protection is unavailable")?;
        let _mutation = self
            .mutation
            .try_lock()
            .map_err(|_| "connections are being changed; retry shortly")?;
        let mut store = Store::open(&self.root).map_err(|error| error.to_string())?;
        let current = store.load().map_err(|error| error.to_string())?;
        if current != before {
            return Err("connections changed during sign-in; try again".into());
        }
        let mut records = current
            .clone()
            .ok_or("connection metadata must be initialized before connecting")?;
        let id = if let Some(target) = target {
            ProviderId::new(target.into_string()).ok_or("invalid ChatGPT connection identity")?
        } else {
            let mut existing = records
                .connections
                .iter()
                .filter(|record| record.family == Family::ChatgptCodex);
            let selected = existing.next().map(|record| record.id.clone());
            if existing.next().is_some() {
                return Err("multiple ChatGPT connections exist; select one with --id".into());
            }
            selected
                .or_else(|| ProviderId::new("chatgpt"))
                .ok_or("invalid ChatGPT connection identity")?
        };
        let existing = records.connections.iter().find(|record| record.id == id);
        if existing.is_some_and(|record| {
            record.family != Family::ChatgptCodex
                || record.catalog_provider.as_str() != "openai"
                || record.method != AuthMethod::OauthDevice
        }) {
            return Err(
                "ChatGPT connection identity is already used by another integration".into(),
            );
        }
        let now = grapt_store::SystemClock.now();
        let (listing, model) = listing(raw_listing, now.as_millis())?;
        let ids =
            grapt_store::OsRandomIds::seed().map_err(|_| "connection identity is unavailable")?;
        let (credential_file, discovery_file) = next_key_files(&id, existing, ids.mint().as_str())
            .map_err(|error| error.to_string())?;
        let oauth_file = format!(
            "{}.oauth.json",
            credential_file.trim_end_matches(".credential")
        );
        let binding = DestinationBinding::oauth(
            ApiDestination::parse(backend).map_err(|error| error.to_string())?,
            OAuthRefreshDestination::parse(issuer).map_err(|error| error.to_string())?,
        );
        let connection = Connection {
            id: id.clone(),
            catalog_provider: ProviderId::new("openai")
                .ok_or("invalid OpenAI provider identity")?,
            family: Family::ChatgptCodex,
            method: AuthMethod::OauthDevice,
            label,
            endpoint: Some(backend.into()),
            watsonx_project: None,
            account: AccountId::new(tokens.account.clone())
                .ok_or("invalid ChatGPT account identity")?,
            route: ModelRouteId::new("responses").ok_or("invalid ChatGPT route")?,
            credential_version: CredentialVersion::new(format!("oauth-{}", ids.mint().as_str()))
                .ok_or("invalid credential version")?,
            policy_version: PolicyVersion::new("connection-setup-v1")
                .ok_or("invalid connection policy")?,
            credential_file,
            discovery_file,
            oauth_file: Some(oauth_file),
            credential_format: CredentialFormat::VaultedOAuth {
                binding: binding.clone(),
            },
        };
        if let Some(existing) = records
            .connections
            .iter_mut()
            .find(|record| record.id == id)
        {
            *existing = connection;
        } else {
            records.connections.push(connection);
        }
        if records.connections.len() == 1 && records.default_model.is_none() {
            records.default_model = Some(DefaultModel {
                provider: id.clone(),
                model: ModelId::new(&model).ok_or("invalid default model")?,
                variant: None,
            });
        } else if let Some(selected) = records
            .default_model
            .as_mut()
            .filter(|selected| selected.provider == id)
        {
            let listing: serde_json::Value =
                serde_json::from_slice(&listing).map_err(|_| "invalid ChatGPT model list")?;
            if !listing["models"].as_array().is_some_and(|rows| {
                rows.iter()
                    .any(|row| row["slug"].as_str() == Some(selected.model.as_str()))
            }) {
                selected.model = ModelId::new(model).ok_or("invalid replacement model")?;
                selected.variant = None;
            }
        }
        let approval = grapt_platform::profile_configuration::ConnectionChange::prepare(
            &self.root,
            current.as_ref(),
            &records,
        )
        .map_err(|error| error.to_string())?;
        let (prepared, commit) = store
            .install_oauth_with(
                &records,
                &id,
                VaultSecret::oauth(tokens).map_err(|error| error.to_string())?,
                &binding,
                issuer,
                &listing,
                now,
                || self.prepare_generation(&records),
            )
            .map_err(|error| error.to_string())?;
        self.publish_generation(prepared, approval, commit)
    }
}
