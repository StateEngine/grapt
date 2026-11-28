//! Synchronous install and generation publication, after network validation.
use super::*;
use grapt_connections::store::{
    next_key_files, AuthMethod, Connection, CredentialFormat, DefaultModel,
};
use grapt_types::{
    AccountId, CredentialVersion, IdAuthority, ModelId, ModelRouteId, PolicyVersion, ProviderId,
};

pub(super) fn connection_id(
    records: Option<&Connections>,
    provider: KeyProvider,
    target: Option<&str>,
) -> Result<ProviderId, String> {
    let stem = provider.connection_stem();
    let occupied = records.and_then(|records| {
        records
            .connections
            .iter()
            .find(|record| record.id.as_str() == stem)
    });
    let id = if let Some(target) = target {
        target.into()
    } else if provider.id() == "custom" {
        (1..=grapt_connections::store::CONNECTION_CAP)
            .map(|index| {
                if index == 1 {
                    "custom".into()
                } else {
                    format!("custom-{index}")
                }
            })
            .find(|id: &String| {
                records.is_none_or(|records| {
                    records
                        .connections
                        .iter()
                        .all(|record| record.id.as_str() != id)
                })
            })
            .ok_or("custom connection limit reached; disconnect one first")?
    } else if occupied.is_some_and(|record| {
        record.family != provider.family() || record.catalog_provider.as_str() != provider.id()
    }) {
        format!("{stem}-key")
    } else {
        stem
    };
    if records.is_some_and(|records| {
        records.connections.iter().any(|record| {
            record.id.as_str() == id
                && (record.family != provider.family()
                    || record.catalog_provider.as_str() != provider.id())
        })
    }) {
        return Err("key connection identity is already used by another integration".into());
    }
    ProviderId::new(id).ok_or_else(|| "invalid key connection identity".into())
}

pub(super) enum KeySource {
    Vault(grapt_login::key::VerifiedKey),
    Environment {
        name: String,
        verified: grapt_login::key::VerifiedKey,
    },
}

pub(super) enum Prepared {
    Pending,
    First(
        Arc<ServerApplication>,
        super::super::prompt_driver::PromptDriver,
    ),
    Update(
        Arc<ServerApplication>,
        super::super::provider_generation::Generation,
    ),
}

impl ConnectionSetup {
    #[allow(clippy::too_many_arguments)]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) fn install(
        &self,
        provider: KeyProvider,
        base: &str,
        label: String,
        before: Option<Connections>,
        source: KeySource,
        connection: ProviderId,
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
            return Err("connections changed while the key was checked; retry".into());
        }
        let mut records = current
            .clone()
            .ok_or("connection metadata must be initialized before connecting")?;
        let connection = connection_id(Some(&records), provider, Some(connection.as_str()))?;
        let (verified, environment) = match source {
            KeySource::Vault(verified) => (verified, None),
            KeySource::Environment { name, verified } => (verified, Some(name)),
        };
        let cloudflare = verified.cloudflare().cloned();
        let watsonx_project = verified.watsonx_project().cloned();
        let (secret, binding, listing) = verified.into_parts();
        let azure_cli = matches!(
            secret.expose(),
            grapt_login::vault::CredentialView::AzureCli
        );
        let google_adc = matches!(
            secret.expose(),
            grapt_login::vault::CredentialView::VertexAdc
        );
        let aws_chain = matches!(
            secret.expose(),
            grapt_login::vault::CredentialView::AwsChain(_)
        );
        let messages_destination = (provider.family()
            == grapt_connections::store::Family::AnthropicMessages
            && provider.placement() == grapt_login::key::KeyPlacement::Bearer)
            .then(|| grapt_login::vault::MessagesDestination::parse(base))
            .transpose()
            .map_err(|_| "invalid Messages endpoint")?;
        let listing = provider.supported_listing(&listing)?;
        let model = provider.default_model(&listing)?;
        let ids =
            grapt_store::OsRandomIds::seed().map_err(|_| "connection identity is unavailable")?;
        let (credential_file, discovery_file) = next_key_files(
            &connection,
            records
                .connections
                .iter()
                .find(|record| record.id == connection),
            ids.mint().as_str(),
        )
        .map_err(|error| error.to_string())?;
        let proposed = Connection {
            id: connection.clone(),
            catalog_provider: ProviderId::new(provider.id()).ok_or("invalid provider identity")?,
            family: provider.family(),
            method: environment.as_ref().map_or(
                if azure_cli {
                    AuthMethod::AzureCli
                } else if google_adc {
                    AuthMethod::GoogleAdc
                } else if aws_chain {
                    AuthMethod::AwsChain
                } else {
                    AuthMethod::Key
                },
                |name| AuthMethod::Env { name: name.clone() },
            ),
            label,
            endpoint: Some(
                messages_destination
                    .as_ref()
                    .map_or(base, |destination| destination.as_str())
                    .into(),
            ),
            watsonx_project,
            account: AccountId::new(if azure_cli {
                "azure-cli"
            } else if google_adc {
                "google-adc"
            } else if aws_chain {
                "aws-chain"
            } else {
                "api-key"
            })
            .ok_or("invalid key account")?,
            route: ModelRouteId::new(provider.route()).ok_or("invalid key route")?,
            credential_version: CredentialVersion::new(format!("key-{}", ids.mint().as_str()))
                .ok_or("invalid credential version")?,
            policy_version: PolicyVersion::new("connection-setup-v1")
                .ok_or("invalid connection policy")?,
            credential_file,
            discovery_file,
            oauth_file: None,
            credential_format: if azure_cli {
                CredentialFormat::AzureCli {
                    binding: binding.clone(),
                }
            } else if google_adc {
                CredentialFormat::GoogleAdc {
                    binding: binding.clone(),
                }
            } else if aws_chain {
                CredentialFormat::AwsChain {
                    binding: binding.clone(),
                }
            } else if environment.is_some() {
                CredentialFormat::LegacyFile
            } else if let Some(scope) = cloudflare {
                CredentialFormat::CloudflareGateway {
                    binding: binding.clone(),
                    scope,
                }
            } else if let Some(destination) = messages_destination {
                CredentialFormat::MessagesBearer {
                    binding: binding.clone(),
                    destination,
                }
            } else {
                CredentialFormat::Vault {
                    binding: binding.clone(),
                }
            },
        };
        if let Some(existing) = records
            .connections
            .iter_mut()
            .find(|record| record.id == connection)
        {
            *existing = proposed;
        } else {
            records.connections.push(proposed);
        }
        if records.connections.len() == 1 && records.default_model.is_none() {
            records.default_model = Some(DefaultModel {
                provider: connection.clone(),
                model: ModelId::new(&model).ok_or("invalid default model")?,
                variant: None,
            });
        } else if let Some(selected) = records
            .default_model
            .as_mut()
            .filter(|selected| selected.provider == connection)
        {
            // A reconnect keeps the chosen model only while it still runs
            // here, and its variant only while the new listing offers it.
            if !provider.runs(&listing, selected.model.as_str())? {
                selected.model = ModelId::new(model).ok_or("invalid replacement model")?;
                selected.variant = None;
            } else if let Some(variant) = &selected.variant {
                if !offers_variant(provider, &listing, selected.model.as_str(), variant)? {
                    selected.variant = None;
                }
            }
        }
        let approval = grapt_platform::profile_configuration::ConnectionChange::prepare(
            &self.root,
            current.as_ref(),
            &records,
        )
        .map_err(|error| error.to_string())?;
        let (prepared, commit) = if environment.is_some() {
            drop(secret);
            store.install_environment_with(&records, &connection, &listing, &binding, || {
                self.prepare_generation(&records)
            })
        } else {
            store.install_key_with(
                &records,
                &connection,
                secret,
                &binding,
                &listing,
                grapt_store::SystemClock.now(),
                || self.prepare_generation(&records),
            )
        }
        .map_err(|error| error.to_string())?;
        self.publish_generation(prepared, approval, commit)
    }

    pub(super) fn prepare_generation(&self, records: &Connections) -> std::io::Result<Prepared> {
        if !self.activate {
            grapt_platform::PublishedConfig::validate_pending_connections(&self.root, records)
                .map_err(std::io::Error::other)?;
            return Ok(Prepared::Pending);
        }
        let application = self.application();
        if records.connections.is_empty() {
            let application = application
                .ok_or_else(|| std::io::Error::other("no execution application is connected"))?;
            let generation = super::super::provider_generation::Generation::disconnected(
                &application.catalog_snapshot(),
            );
            return Ok(Prepared::Update(application, generation));
        }
        let endpoint =
            super::super::production::provider_endpoint().map_err(std::io::Error::other)?;
        if let Some(application) = application {
            let executor = &application.executor.0;
            let inputs = grapt_platform::PreparedProviderInputs::prepare_pending_connections(
                &self.root, records,
            )
            .map_err(std::io::Error::other)?;
            let generation = executor
                .prepare_provider_generation(inputs, &endpoint, self.limits)
                .map_err(std::io::Error::other)?;
            Ok(Prepared::Update(application, generation))
        } else {
            let platform =
                grapt_platform::PlatformOwner::prepare_pending_connections(&self.root, records)
                    .map_err(std::io::Error::other)?;
            let mut parts = platform
                .into_provider_parts()
                .map_err(std::io::Error::other)?;
            let secondary = parts.take_secondary_credentials();
            let (mut event_store, targets, _build, credential, _first_contact) =
                parts.into_components();
            crate::commit_executor::install(&mut event_store);
            let (application, driver, _plugins) =
                crate::dogfood_composition::open_production_application(
                    &self.root,
                    event_store,
                    targets,
                    credential,
                    secondary,
                    endpoint,
                    self.limits,
                    Arc::clone(&self.hub),
                    self.external_host,
                )
                .map_err(std::io::Error::other)?;
            Ok(Prepared::First(application, driver))
        }
    }

    pub(super) fn publish_generation(
        &self,
        prepared: Prepared,
        approval: grapt_platform::profile_configuration::ConnectionChange,
        commit: grapt_connections::store::InstallCommit,
    ) -> Result<(), String> {
        if let Err(error) = approval.commit() {
            crate::diagnostics::report(format!(
                "connection metadata changed; profile approval update refused ({:?})",
                error.kind()
            ));
        }
        match prepared {
            Prepared::Pending => (),
            Prepared::Update(application, generation) => application.providers.publish(generation),
            Prepared::First(application, driver) => {
                self.ports
                    .install(application.clone(), application.clone())?;
                match self.application.lock() {
                    Ok(mut current) => *current = Some(application),
                    Err(poisoned) => *poisoned.into_inner() = Some(application),
                }
                let sender = match self.driver.lock() {
                    Ok(mut sender) => sender.take(),
                    Err(poisoned) => poisoned.into_inner().take(),
                }
                .ok_or("provider activation owner is unavailable")?;
                sender
                    .send(driver)
                    .map_err(|_| "service stopped during provider activation")?;
            }
        }
        if let Some(reason) = commit.cleanup_error {
            crate::diagnostics::report(format!(
                "connection metadata changed; staging cleanup will retry on next change ({reason:?})"
            ));
        }
        if let Some(reason) = commit.durability_error {
            crate::diagnostics::report(format!(
                "connection metadata changed; metadata directory durability is uncertain ({reason:?})"
            ));
        }
        Ok(())
    }
}

/// Whether the listing row for `model` offers `variant`, read as the model
/// inventory reads it.
fn offers_variant(
    provider: KeyProvider,
    listing: &[u8],
    model: &str,
    variant: &grapt_types::ModelVariantId,
) -> Result<bool, String> {
    let parsed: serde_json::Value =
        serde_json::from_slice(listing).map_err(|_| "invalid verified model list")?;
    let Some(row) = parsed["data"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"].as_str() == Some(model)))
    else {
        return Ok(false);
    };
    Ok(grapt_platform::offered_variants(provider.id(), model, row)?.contains(variant))
}

#[cfg(test)]
mod variant_tests {
    use super::*;

    #[test]
    fn a_variant_stays_only_while_the_live_row_offers_it() -> Result<(), String> {
        let provider = KeyProvider::from_id("custom")?.ok_or("custom provider")?;
        let listing = br#"{"data":[{"id":"local","max_model_len":65536,
            "supported_reasoning_levels":[{"effort":"low"}]}]}"#;
        let variant = |name: &str| grapt_types::ModelVariantId::new(name).ok_or("variant");
        assert!(offers_variant(
            provider,
            listing,
            "local",
            &variant("low")?
        )?);
        assert!(!offers_variant(
            provider,
            listing,
            "local",
            &variant("high")?
        )?);
        assert!(!offers_variant(
            provider,
            listing,
            "absent",
            &variant("low")?
        )?);
        Ok(())
    }
}
