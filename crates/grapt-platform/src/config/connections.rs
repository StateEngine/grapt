//! Publish the service-owned connection generation through existing grants.
use super::*;
use grapt_connections::store::{AuthMethod, Connections, Family, Store};

pub(super) fn read(root: &Path) -> Result<Option<Connections>, ConfigPublicationError> {
    use crate::profile_configuration::{frozen, FrozenRead};
    let captured =
        frozen(&root.join(grapt_connections::store::FILE), 64 * 1024).map_err(refused)?;
    match captured {
        FrozenRead::Bytes(bytes) => Connections::from_slice(&bytes).map(Some),
        FrozenRead::Missing => Ok(None),
        FrozenRead::Unbound => Store::read_snapshot(root),
    }
    .map_err(refused)
}

fn refused(error: std::io::Error) -> ConfigPublicationError {
    // No document bytes, endpoint or credential can enter this diagnostic.
    ConfigPublicationError::Load(format!("connection storage refused: {:?}", error.kind()))
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn catalog(
    resolution: &Resolution<'_>,
    connections: Connections,
) -> Result<CatalogSpec, ConfigPublicationError> {
    let selected = connections
        .effective_selection(resolution)
        .map_err(|error| ConfigPublicationError::Load(error.to_string()))?
        .ok_or(ConfigPublicationError::Missing {
            path: "$.default_model",
        })?;
    let mut primary = None;
    let mut secondary = Vec::new();
    for connection in connections.connections {
        let family = match connection.family {
            Family::ChatgptCodex => ProtocolFamily::ChatGptCodex,
            Family::OpenaiResponses => ProtocolFamily::OpenAiResponses,
            Family::AnthropicMessages => ProtocolFamily::AnthropicMessages,
            Family::VertexAnthropic => ProtocolFamily::VertexAnthropic,
            Family::OpenaiCompatible => ProtocolFamily::OpenAiCompatible,
            Family::BedrockConverse => ProtocolFamily::BedrockConverse,
            Family::GoogleGemini => ProtocolFamily::GoogleGemini,
            Family::CohereChat => ProtocolFamily::CohereChat,
            Family::VertexGemini => ProtocolFamily::VertexGemini,
            Family::WatsonxChat => ProtocolFamily::WatsonxChat,
        };
        if let Some(binding) = connection.credential_format.binding() {
            let endpoint = connection
                .endpoint
                .as_deref()
                .or(grapt_provider::family_default_endpoint(family))
                .ok_or_else(|| {
                    ConfigPublicationError::EndpointRequired(connection.id.as_str().into())
                })?;
            binding
                .check_api(endpoint)
                .map_err(|error| ConfigPublicationError::Load(error.to_string()))?;
        }
        let account = AccountSpec {
            watsonx_project: connection.watsonx_project,
            chatgpt_files: if connection.method == AuthMethod::OauthDevice {
                Some({
                    let files = grapt_login::chatgpt::SignInFiles::new(
                        connection.oauth_file.as_deref().ok_or_else(|| {
                            ConfigPublicationError::Load("ChatGPT state reference is absent".into())
                        })?,
                        &connection.credential_file,
                    )
                    .map_err(|error| ConfigPublicationError::Load(error.to_string()))?;
                    match &connection.credential_format {
                        grapt_connections::store::CredentialFormat::MessagesBearer { .. }
                        | grapt_connections::store::CredentialFormat::GoogleAdc { .. }
                        | grapt_connections::store::CredentialFormat::AwsChain { .. }
                        | grapt_connections::store::CredentialFormat::AzureCli { .. } => {
                            return Err(ConfigPublicationError::Load(
                                "Messages bearer credentials cannot authorize OAuth".into(),
                            ));
                        }
                        grapt_connections::store::CredentialFormat::CloudflareGateway {
                            ..
                        } => {
                            return Err(ConfigPublicationError::Load(
                                "Cloudflare credentials cannot authorize OAuth".into(),
                            ));
                        }
                        grapt_connections::store::CredentialFormat::LegacyFile => files,
                        grapt_connections::store::CredentialFormat::Vault { binding } => files
                            .with_vault(connection.id.clone(), binding.clone())
                            .map_err(|error| ConfigPublicationError::Load(error.to_string()))?,
                        grapt_connections::store::CredentialFormat::VaultedOAuth { binding } => {
                            files
                                .with_vault(connection.id.clone(), binding.clone())
                                .and_then(|files| {
                                    files.with_generation(connection.credential_version.clone())
                                })
                                .map_err(|error| ConfigPublicationError::Load(error.to_string()))?
                        }
                    }
                })
            } else {
                None
            },
            credential_format: connection.credential_format,
            provider: connection.id,
            catalog_provider: connection.catalog_provider,
            label: Some(connection.label),
            routing: RoutingRequest {
                route: connection.route,
                account: connection.account,
                credential_version: connection.credential_version,
                policy: connection.policy_version,
            },
            protocol_family: family,
            credential_source: crate::credential::CredentialSourceName::new(
                &connection.credential_file,
            )
            .ok_or_else(|| {
                ConfigPublicationError::Load("invalid connection credential reference".into())
            })?,
            credential_environment: match connection.method {
                AuthMethod::Env { name } => Some(name),
                _ => None,
            },
            discovery: connection.discovery_file,
            endpoint: connection.endpoint,
        };
        if family == ProtocolFamily::OpenAiCompatible && account.endpoint.is_none() {
            return Err(ConfigPublicationError::EndpointRequired(
                account.provider.as_str().into(),
            ));
        }
        if account.provider == selected.provider {
            primary = Some(account);
        } else {
            secondary.push(account);
        }
    }
    let primary = primary.ok_or_else(|| {
        ConfigPublicationError::UnconfiguredProvider(selected.provider.as_str().into())
    })?;
    // Discovery supplies catalog model identities. These fallback coordinates
    // serve only provider-list inventories without composed catalog metadata.
    let base_model = id(
        BaseModelId::new(selected.model.as_str()),
        "$.default_model.id",
    )?;
    let family = id(
        ModelFamilyId::new(selected.model.as_str()),
        "$.default_model.id",
    )?;
    Ok(CatalogSpec {
        managed_connections: true,
        model: selected.model,
        base_model,
        family,
        variants: Vec::new(),
        selected_variant: selected.variant,
        primary,
        secondary,
        inventory: None,
    })
}

impl PublishedConfig {
    /// Validate an operator's proposed catalog without opening credential material.
    /// Empty generations have no target; their build/store header was checked at launch.
    pub fn validate_pending_connections(
        root: &Path,
        records: &Connections,
    ) -> Result<(), ConfigPublicationError> {
        records.to_vec().map_err(refused)?;
        if !records.connections.is_empty() {
            Self::load_pending_connections(root, records)?;
        }
        Ok(())
    }

    /// Bootstrap the managed store from this already validated configuration.
    /// Ordinary configuration reads stay read-only. A concurrent, different
    /// generation refuses; captured legacy input can never overwrite it.
    pub fn ensure_connection_store(&self) -> Result<bool, ConfigPublicationError> {
        if self.catalog.managed_connections {
            return Ok(false);
        }
        let records = Connections {
            connections: self
                .catalog
                .accounts()
                .map(|account| {
                    let family = match account.protocol_family {
                        ProtocolFamily::ChatGptCodex => Family::ChatgptCodex,
                        ProtocolFamily::OpenAiResponses => Family::OpenaiResponses,
                        ProtocolFamily::AnthropicMessages => Family::AnthropicMessages,
                        ProtocolFamily::VertexAnthropic => Family::VertexAnthropic,
                        ProtocolFamily::OpenAiCompatible => Family::OpenaiCompatible,
                        ProtocolFamily::BedrockConverse => Family::BedrockConverse,
                        ProtocolFamily::GoogleGemini => Family::GoogleGemini,
                        ProtocolFamily::CohereChat => Family::CohereChat,
                        ProtocolFamily::VertexGemini => Family::VertexGemini,
                        ProtocolFamily::WatsonxChat => Family::WatsonxChat,
                    };
                    let method = match &account.credential_environment {
                        Some(name) => AuthMethod::Env { name: name.clone() },
                        None if account.chatgpt_files.is_some() => AuthMethod::OauthDevice,
                        None => AuthMethod::Key,
                    };
                    grapt_connections::store::Connection {
                        id: account.provider.clone(),
                        catalog_provider: account.catalog_provider.clone(),
                        label: account.label.clone().unwrap_or_else(|| {
                            if family == Family::ChatgptCodex {
                                "ChatGPT".into()
                            } else {
                                account.provider.as_str().into()
                            }
                        }),
                        family,
                        oauth_file: (method == AuthMethod::OauthDevice)
                            .then(|| grapt_login::chatgpt::STATE_FILE.into()),
                        method,
                        endpoint: account.endpoint.clone(),
                        watsonx_project: account.watsonx_project.clone(),
                        account: account.routing.account.clone(),
                        route: account.routing.route.clone(),
                        credential_version: account.routing.credential_version.clone(),
                        policy_version: account.routing.policy.clone(),
                        credential_file: account.credential_source.as_str().into(),
                        credential_format: account.credential_format.clone(),
                        discovery_file: account.discovery.clone(),
                    }
                })
                .collect(),
            default_model: Some(grapt_connections::store::DefaultModel {
                provider: self.catalog.provider().clone(),
                model: self.catalog.model.clone(),
                variant: self.catalog.selected_variant.clone(),
            }),
        };
        // This explicit startup writer establishes the private profile boundary;
        // loading an existing managed generation never repairs its permissions.
        grapt_login::profile_security::protect_directory(&self.service_root).map_err(refused)?;
        let mut store = Store::open(&self.service_root).map_err(refused)?;
        if let Some(current) = store.load().map_err(refused)? {
            if current != records {
                return Err(ConfigPublicationError::Load(
                    "connections changed during legacy migration; restart with the current profile"
                        .into(),
                ));
            }
            crate::profile_configuration::record_connection_migration(
                &self.service_root,
                &records.to_vec().map_err(refused)?,
            )
            .map_err(refused)?;
            return Ok(false);
        }
        store.save(&records).map_err(refused)?;
        crate::profile_configuration::record_connection_migration(
            &self.service_root,
            &records.to_vec().map_err(refused)?,
        )
        .map_err(refused)?;
        Ok(true)
    }
}

/// Retain matching configured model metadata across first-touch migration.
/// Account/route/credential authority always comes from connections.
pub(super) fn configuration_only(
    spec: &mut PlatformSpec,
    resolution: &Resolution<'_>,
    root: &Path,
) -> Result<bool, ConfigPublicationError> {
    if !spec.catalog.managed_connections
        || nearest(resolution, &["provider", "id"]).and_then(ConfigValue::as_str)
            != Some(spec.catalog.provider().as_str())
        || nearest(resolution, &["provider", "model"]).and_then(ConfigValue::as_str)
            != Some(spec.catalog.model.as_str())
    {
        return Ok(false);
    }
    if let Some(value) = optional_string(
        resolution,
        &["provider", "base_model"],
        "$.provider.base_model",
    )? {
        spec.catalog.base_model = id(BaseModelId::new(value), "$.provider.base_model")?;
    }
    if let Some(value) = optional_string(resolution, &["provider", "family"], "$.provider.family")?
    {
        spec.catalog.family = id(ModelFamilyId::new(value), "$.provider.family")?;
    }
    spec.catalog.variants = super::derive::variants(resolution)?;
    if matches!(
        spec.catalog.primary.protocol_family,
        ProtocolFamily::OpenAiResponses
            | ProtocolFamily::OpenAiCompatible
            | ProtocolFamily::CohereChat
    ) || nearest(resolution, &["catalog", "source"]).is_some()
    {
        return Ok(false);
    }
    match std::fs::symlink_metadata(root.join(&spec.catalog.primary.discovery)) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(refused(error)),
    }
}
