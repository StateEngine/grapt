//! Deriving a platform spec from resolved configuration.
//!
//! Split out of `config.rs` under the workspace god-file ceiling. `config.rs`
//! owns the published types and the authority they carry; this owns turning
//! resolved configuration INTO them, which is where every refusal about a
//! malformed or absent field lives.
//!
//! A private sibling: the glob keeps the split a move rather than an interface
//! change, and nothing here is public beyond the crate.

use super::*;

pub(super) fn config_inputs(
    service_root: &Path,
) -> Result<(grapt_config::DiscoveryPlan, ConfigBounds, SourceBounds), ConfigPublicationError> {
    let document_bytes = MaxDocumentBytes::try_new(64 * 1_024)
        .map_err(|error| ConfigPublicationError::Bounds(error.to_string()))?;
    let ancestor_depth = MaxAncestorDepth::try_new(8)
        .map_err(|error| ConfigPublicationError::Bounds(error.to_string()))?;
    let documents = MaxDocuments::try_new(9)
        .map_err(|error| ConfigPublicationError::Bounds(error.to_string()))?;
    let value_depth = MaxValueDepth::try_new(16)
        .map_err(|error| ConfigPublicationError::Bounds(error.to_string()))?;
    let document_nodes = MaxDocumentNodes::try_new(4_096)
        .map_err(|error| ConfigPublicationError::Bounds(error.to_string()))?;
    let path_bytes = MaxPathBytes::try_new(MaxPathBytes::CEILING)
        .map_err(|error| ConfigPublicationError::Bounds(error.to_string()))?;
    let bounds = ConfigBounds::new(
        document_bytes,
        ancestor_depth,
        documents,
        value_depth,
        document_nodes,
    );
    let source_bounds = SourceBounds::from_config_bounds(bounds, path_bytes)
        .map_err(|error| ConfigPublicationError::Bounds(error.to_string()))?;
    let start = ConfigSourcePath::new(service_root, path_bytes)
        .map_err(|error| ConfigPublicationError::Path(error.to_string()))?;
    // A profile made before the rename keeps reading its legacy file name.
    let spelling = crate::profile_configuration::profile_spelling(service_root)
        .map_err(|error| ConfigPublicationError::Path(error.to_string()))?
        .spelling;
    let file_name = ConfigFileName::new(spelling.config_jsonc())
        .ok_or_else(|| ConfigPublicationError::Path("invalid fixed file name".to_owned()))?;
    Ok((
        grapt_config::DiscoveryPlan::new(start, None, file_name),
        bounds,
        source_bounds,
    ))
}

pub(super) fn derive_spec(
    resolution: &Resolution<'_>,
    root: &Path,
) -> Result<PlatformSpec, ConfigPublicationError> {
    derive_spec_with_connections(resolution, root, None)
}

pub(super) fn derive_spec_with_connections(
    resolution: &Resolution<'_>,
    root: &Path,
    proposed: Option<grapt_connections::store::Connections>,
) -> Result<PlatformSpec, ConfigPublicationError> {
    let legacy_store_authority = legacy_store_authority(resolution)?;
    let build = required_string(resolution, &["product", "build"], "$.product.build")?;
    let catalog = match proposed {
        Some(connections) => super::connections::catalog(resolution, connections)?,
        None => match super::connections::read(root)? {
            Some(connections) => super::connections::catalog(resolution, connections)?,
            None => legacy_catalog(resolution)?,
        },
    };
    Ok(PlatformSpec {
        legacy_store_authority,
        build,
        catalog,
    })
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn legacy_catalog(
    resolution: &Resolution<'_>,
) -> Result<CatalogSpec, ConfigPublicationError> {
    let provider = id(
        ProviderId::new(required_string(
            resolution,
            &["provider", "id"],
            "$.provider.id",
        )?),
        "$.provider.id",
    )?;
    let model = id(
        ModelId::new(required_string(
            resolution,
            &["provider", "model"],
            "$.provider.model",
        )?),
        "$.provider.model",
    )?;
    let base_model = id(
        BaseModelId::new(required_string(
            resolution,
            &["provider", "base_model"],
            "$.provider.base_model",
        )?),
        "$.provider.base_model",
    )?;
    let family = id(
        ModelFamilyId::new(required_string(
            resolution,
            &["provider", "family"],
            "$.provider.family",
        )?),
        "$.provider.family",
    )?;
    let route = id(
        ModelRouteId::new(required_string(
            resolution,
            &["provider", "route"],
            "$.provider.route",
        )?),
        "$.provider.route",
    )?;
    let account = id(
        AccountId::new(required_string(
            resolution,
            &["provider", "account"],
            "$.provider.account",
        )?),
        "$.provider.account",
    )?;
    let credential_version = id(
        CredentialVersion::new(required_string(
            resolution,
            &["provider", "credential_version"],
            "$.provider.credential_version",
        )?),
        "$.provider.credential_version",
    )?;
    let policy = id(
        PolicyVersion::new(required_string(
            resolution,
            &["provider", "policy_version"],
            "$.provider.policy_version",
        )?),
        "$.provider.policy_version",
    )?;
    let variants = variants(resolution)?;
    let selected_variant = optional_string(
        resolution,
        &["provider", "selected_variant"],
        "$.provider.selected_variant",
    )?
    .map(|value| id(ModelVariantId::new(value), "$.provider.selected_variant"))
    .transpose()?;
    if let Some(selected) = &selected_variant {
        if !variants.iter().any(|variant| variant == selected)
            && optional_string(resolution, &["catalog", "source"], "$.catalog.source")?.is_none()
        {
            return Err(ConfigPublicationError::SelectedVariantAbsent(
                selected.as_str().to_owned(),
            ));
        }
    } else if !variants.is_empty() {
        return Err(ConfigPublicationError::Missing {
            path: "$.provider.selected_variant",
        });
    }
    // OPTIONAL, and absent means exactly what it always meant: the primary
    // connection keeps the family default it always used. Only a configuration
    // that explicitly says otherwise moves it, and it says so through the same
    // resolution and the same validation every other optional string uses —
    // present-but-empty and wrong-type stay explicit refusals, never a silent
    // fallback to the default.
    //
    // Until this existed the primary was built with `endpoint: None` and no way
    // to say otherwise, so no configuration could point it at a local fixture.
    // That is not an abstract gap: a qualification turn whose model selection
    // failed to bind fell back to the primary and left the host for a real
    // provider address, and no configuration could have prevented it.
    let endpoint = optional_string(resolution, &["provider", "endpoint"], "$.provider.endpoint")?;
    // OPTIONAL, for the same reason as the endpoint: absent keeps the family
    // the primary always had, so every existing profile publishes unchanged.
    // An API-key first run (J01) declares `anthropic-messages` or
    // `openai-responses` here, validated exactly as an additional account's.
    let primary_family = optional_string(
        resolution,
        &["provider", "protocol_family"],
        "$.provider.protocol_family",
    )?
    .map(|value| protocol_family(&value))
    .transpose()?
    .unwrap_or(ProtocolFamily::ChatGptCodex);
    let credential_environment = optional_string(
        resolution,
        &["provider", "credential"],
        "$.provider.credential",
    )?
    .map(|value| environment_reference(&value))
    .transpose()?;
    let primary = AccountSpec {
        watsonx_project: None,
        credential_format: grapt_connections::store::CredentialFormat::LegacyFile,
        chatgpt_files: (primary_family == ProtocolFamily::ChatGptCodex
            && credential_version.as_str() == grapt_login::chatgpt::SIGN_IN_CREDENTIAL_VERSION
            && credential_environment.is_none())
        .then(grapt_login::chatgpt::SignInFiles::legacy),
        catalog_provider: optional_string(
            resolution,
            &["provider", "catalog_provider"],
            "$.provider.catalog_provider",
        )?
        .map(|value| id(ProviderId::new(value), "$.provider.catalog_provider"))
        .transpose()?
        .unwrap_or_else(|| provider.clone()),
        label: optional_string(resolution, &["provider", "label"], "$.provider.label")?,
        provider,
        routing: RoutingRequest {
            route,
            account,
            credential_version,
            policy,
        },
        protocol_family: primary_family,
        credential_source: crate::credential::CredentialSourceName::primary(),
        credential_environment,
        discovery: PRIMARY_DISCOVERY_FILE.to_owned(),
        endpoint,
    };
    let secondary = secondary_accounts(resolution, &primary)?;
    Ok(CatalogSpec {
        managed_connections: false,
        inventory: None,
        model,
        base_model,
        family,
        variants,
        selected_variant,
        primary,
        secondary,
    })
}

/// Compose the optional additional connections at `$.provider.accounts`.
///
/// Absent means exactly one connection, which is the established single-account
/// configuration unchanged. Each admitted entry names its own provider, routing
/// coordinates, protocol family and bounded local inputs; nothing is inherited
/// from the primary account, because inheriting a route, account or credential
/// version is how a second provider would silently borrow the first one's
/// authority.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn secondary_accounts(
    resolution: &Resolution<'_>,
    primary: &AccountSpec,
) -> Result<Vec<AccountSpec>, ConfigPublicationError> {
    let Some(value) = nearest(resolution, &["provider", "accounts"]) else {
        return Ok(Vec::new());
    };
    let ConfigValue::Array(values) = value else {
        return Err(ConfigPublicationError::WrongType {
            path: "$.provider.accounts",
            expected: "an array of account objects",
        });
    };
    if values.len() > MAX_SECONDARY_ACCOUNTS {
        return Err(ConfigPublicationError::AccountCount {
            actual: values.len(),
            bound: MAX_SECONDARY_ACCOUNTS,
        });
    }
    let mut admitted: Vec<AccountSpec> = Vec::with_capacity(values.len());
    for (index, entry) in values.iter().enumerate() {
        let ConfigValue::Object(fields) = entry.value() else {
            return Err(ConfigPublicationError::WrongType {
                path: "$.provider.accounts[]",
                expected: "an object",
            });
        };
        let provider = id(
            ProviderId::new(account_string(fields, "id", "$.provider.accounts[].id")?),
            "$.provider.accounts[].id",
        )?;
        if provider == primary.provider
            || admitted.iter().any(|account| account.provider == provider)
        {
            let existing = if provider == primary.provider {
                "the primary".to_owned()
            } else {
                match admitted
                    .iter()
                    .position(|account| account.provider == provider)
                {
                    Some(position) => format!("accounts[{position}]"),
                    None => "the primary".to_owned(),
                }
            };
            return Err(ConfigPublicationError::DuplicateAccount {
                provider: provider.as_str().to_owned(),
                entry: format!("accounts[{index}]"),
                existing,
                input: "provider id",
            });
        }
        let route = id(
            ModelRouteId::new(account_string(
                fields,
                "route",
                "$.provider.accounts[].route",
            )?),
            "$.provider.accounts[].route",
        )?;
        let account = id(
            AccountId::new(account_string(
                fields,
                "account",
                "$.provider.accounts[].account",
            )?),
            "$.provider.accounts[].account",
        )?;
        let credential_version = id(
            CredentialVersion::new(account_string(
                fields,
                "credential_version",
                "$.provider.accounts[].credential_version",
            )?),
            "$.provider.accounts[].credential_version",
        )?;
        let policy = id(
            PolicyVersion::new(account_string(
                fields,
                "policy_version",
                "$.provider.accounts[].policy_version",
            )?),
            "$.provider.accounts[].policy_version",
        )?;
        let protocol_family = protocol_family(&account_string(
            fields,
            "protocol_family",
            "$.provider.accounts[].protocol_family",
        )?)?;
        let credential_source = crate::credential::CredentialSourceName::new(&account_string(
            fields,
            "credential",
            "$.provider.accounts[].credential",
        )?)
        .ok_or(ConfigPublicationError::Empty {
            path: "$.provider.accounts[].credential",
        })?;
        if credential_source == primary.credential_source
            || admitted
                .iter()
                .any(|account| account.credential_source == credential_source)
        {
            let existing = if credential_source == primary.credential_source {
                "the primary".to_owned()
            } else {
                match admitted
                    .iter()
                    .position(|account| account.credential_source == credential_source)
                {
                    Some(position) => format!("accounts[{position}]"),
                    None => "the primary".to_owned(),
                }
            };
            return Err(ConfigPublicationError::DuplicateAccount {
                provider: provider.as_str().to_owned(),
                entry: format!("accounts[{index}]"),
                existing,
                input: "credential source",
            });
        }
        let endpoint =
            optional_account_string(fields, "endpoint", "$.provider.accounts[].endpoint")?;
        // Checked here, at composition, rather than at the first attempt: a
        // provider that cannot be reached is a configuration fact, and
        // discovering it only when a user finally selects a model would make
        // an unusable account look configured in the picker.
        if endpoint.is_none() && grapt_provider::family_default_endpoint(protocol_family).is_none()
        {
            return Err(ConfigPublicationError::EndpointRequired(
                provider.as_str().to_owned(),
            ));
        }
        let discovery = account_string(fields, "discovery", "$.provider.accounts[].discovery")?;
        if discovery == primary.discovery
            || admitted
                .iter()
                .any(|account| account.discovery == discovery)
        {
            let existing = if discovery == primary.discovery {
                "the primary".to_owned()
            } else {
                match admitted
                    .iter()
                    .position(|account| account.discovery == discovery)
                {
                    Some(position) => format!("accounts[{position}]"),
                    None => "the primary".to_owned(),
                }
            };
            return Err(ConfigPublicationError::DuplicateAccount {
                provider: provider.as_str().to_owned(),
                entry: format!("accounts[{index}]"),
                existing,
                input: "discovery file",
            });
        }
        admitted.push(AccountSpec {
            watsonx_project: None,
            credential_format: grapt_connections::store::CredentialFormat::LegacyFile,
            chatgpt_files: None,
            catalog_provider: optional_account_string(
                fields,
                "catalog_provider",
                "$.provider.accounts[].catalog_provider",
            )?
            .map(|value| {
                id(
                    ProviderId::new(value),
                    "$.provider.accounts[].catalog_provider",
                )
            })
            .transpose()?
            .unwrap_or_else(|| provider.clone()),
            label: optional_account_string(fields, "label", "$.provider.accounts[].label")?,
            provider,
            routing: RoutingRequest {
                route,
                account,
                credential_version,
                policy,
            },
            protocol_family,
            credential_source,
            credential_environment: None,
            discovery,
            endpoint,
        });
    }
    Ok(admitted)
}

/// An optional account field. Absent is absent; present-but-empty is a
/// refusal, because an empty endpoint would resolve to nothing at attempt time.
pub(super) fn optional_account_string(
    fields: &[(
        grapt_config::Spanned<grapt_config::ConfigKey>,
        grapt_config::Spanned<ConfigValue>,
    )],
    key: &str,
    rendered: &'static str,
) -> Result<Option<String>, ConfigPublicationError> {
    let Some(value) = fields
        .iter()
        .find(|(name, _)| name.value().as_str() == key)
        .map(|(_, value)| value.value())
    else {
        return Ok(None);
    };
    let value = value.as_str().ok_or(ConfigPublicationError::WrongType {
        path: rendered,
        expected: "a string",
    })?;
    if value.is_empty() {
        return Err(ConfigPublicationError::Empty { path: rendered });
    }
    Ok(Some(value.to_owned()))
}

/// The variable an `env:NAME` credential reference names. Only a plain
/// variable name is accepted, so the reference can never smuggle a value.
fn environment_reference(value: &str) -> Result<String, ConfigPublicationError> {
    let refused = || ConfigPublicationError::CredentialReference;
    let name = value.strip_prefix("env:").ok_or_else(refused)?;
    let plain = !name.is_empty()
        && name.len() <= 64
        && !name.as_bytes()[0].is_ascii_digit()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_');
    if plain {
        Ok(name.to_owned())
    } else {
        Err(refused())
    }
}

/// A protocol family's spelling in configuration.
fn family_spelling(family: ProtocolFamily) -> &'static str {
    match family {
        ProtocolFamily::OpenAiCompatible => "openai-compatible",
        ProtocolFamily::BedrockConverse => "bedrock-converse",
        ProtocolFamily::CohereChat => "cohere-chat",
        ProtocolFamily::OpenAiResponses => "openai-responses",
        ProtocolFamily::AnthropicMessages => "anthropic-messages",
        ProtocolFamily::VertexAnthropic => "vertex-anthropic",
        ProtocolFamily::GoogleGemini => "google-gemini",
        ProtocolFamily::VertexGemini => "vertex-gemini",
        ProtocolFamily::WatsonxChat => "watsonx-chat",
        ProtocolFamily::ChatGptCodex => "chatgpt-codex",
    }
}

/// A configured protocol family, by its spelling in configuration.
fn protocol_family(raw: &str) -> Result<ProtocolFamily, ConfigPublicationError> {
    match raw {
        "openai-compatible" => Ok(ProtocolFamily::OpenAiCompatible),
        "bedrock-converse" => Ok(ProtocolFamily::BedrockConverse),
        "cohere-chat" => Ok(ProtocolFamily::CohereChat),
        "openai-responses" => Ok(ProtocolFamily::OpenAiResponses),
        "anthropic-messages" => Ok(ProtocolFamily::AnthropicMessages),
        "vertex-anthropic" => Ok(ProtocolFamily::VertexAnthropic),
        "google-gemini" => Ok(ProtocolFamily::GoogleGemini),
        "vertex-gemini" => Ok(ProtocolFamily::VertexGemini),
        "watsonx-chat" => Ok(ProtocolFamily::WatsonxChat),
        "chatgpt-codex" => Ok(ProtocolFamily::ChatGptCodex),
        other => Err(ConfigPublicationError::ProtocolFamily(other.to_owned())),
    }
}

pub(super) fn account_string(
    fields: &[(
        grapt_config::Spanned<grapt_config::ConfigKey>,
        grapt_config::Spanned<ConfigValue>,
    )],
    key: &str,
    rendered: &'static str,
) -> Result<String, ConfigPublicationError> {
    let value = fields
        .iter()
        .find(|(name, _)| name.value().as_str() == key)
        .map(|(_, value)| value.value())
        .ok_or(ConfigPublicationError::Missing { path: rendered })?;
    let value = value.as_str().ok_or(ConfigPublicationError::WrongType {
        path: rendered,
        expected: "a string",
    })?;
    if value.is_empty() {
        return Err(ConfigPublicationError::Empty { path: rendered });
    }
    Ok(value.to_owned())
}

/// Every configured account's non-secret grant, primary first. A grant is what
/// a credential load is checked against; composing them here keeps the typed
/// account and credential version bound to the account that declared them.
pub(super) fn grants(spec: &CatalogSpec) -> Result<CredentialGrants, ConfigPublicationError> {
    let record = CredentialRecordId::new(LOCAL_DOGFOOD_CREDENTIAL_RECORD)
        .ok_or(ConfigPublicationError::LocalCredentialRecord)?;
    let grant = |account: &AccountSpec| CredentialGrant {
        record: record.clone(),
        provider: account.provider.clone(),
        protocol_family: account.protocol_family,
        route: account.routing.route.clone(),
        account: account.routing.account.clone(),
        version: account.routing.credential_version.clone(),
        placement: if matches!(
            account.credential_format,
            grapt_connections::store::CredentialFormat::GoogleAdc { .. }
        ) {
            CredentialPlacement::VertexAdc
        } else if matches!(
            account.credential_format,
            grapt_connections::store::CredentialFormat::AwsChain { .. }
        ) {
            CredentialPlacement::AwsSigV4
        } else if matches!(
            account.credential_format,
            grapt_connections::store::CredentialFormat::AzureCli { .. }
        ) {
            CredentialPlacement::AzureEntra
        } else if account.credential_format.cloudflare().is_some()
            && matches!(
                account.protocol_family,
                ProtocolFamily::OpenAiResponses | ProtocolFamily::AnthropicMessages
            )
        {
            CredentialPlacement::CloudflareGateway
        } else {
            match account.protocol_family {
                ProtocolFamily::AnthropicMessages => {
                    if matches!(
                        account.credential_format,
                        grapt_connections::store::CredentialFormat::MessagesBearer { .. }
                    ) {
                        CredentialPlacement::BearerAuthorization
                    } else {
                        CredentialPlacement::AnthropicApiKey
                    }
                }
                ProtocolFamily::OpenAiResponses
                | ProtocolFamily::ChatGptCodex
                | ProtocolFamily::OpenAiCompatible
                | ProtocolFamily::BedrockConverse
                | ProtocolFamily::CohereChat
                | ProtocolFamily::VertexAnthropic
                | ProtocolFamily::VertexGemini
                | ProtocolFamily::WatsonxChat => CredentialPlacement::BearerAuthorization,
                ProtocolFamily::GoogleGemini => CredentialPlacement::GoogleApiKey,
            }
        },
        storage: if account.credential_environment.is_some() {
            CredentialStorageMode::ProcessEnvironment
        } else if cfg!(windows) && account.credential_format.binding().is_some() {
            CredentialStorageMode::EncryptedFile
        } else {
            CredentialStorageMode::PlaintextFile
        },
        source: account.credential_source.clone(),
        credential_format: account.credential_format.clone(),
        environment: account.credential_environment.clone(),
        chatgpt_files: account.chatgpt_files.clone(),
    };
    Ok(CredentialGrants {
        primary: grant(&spec.primary),
        secondary: spec.secondary.iter().map(grant).collect(),
    })
}

pub(super) fn variants(
    resolution: &Resolution<'_>,
) -> Result<Vec<ModelVariantId>, ConfigPublicationError> {
    let Some(value) = nearest(resolution, &["provider", "variants"]) else {
        return Ok(Vec::new());
    };
    let ConfigValue::Array(values) = value else {
        return Err(ConfigPublicationError::WrongType {
            path: "$.provider.variants",
            expected: "an array of strings",
        });
    };
    let effective_rows = values.len().max(1);
    require_catalog_rows(effective_rows)?;
    let mut seen = BTreeSet::new();
    let mut admitted = Vec::with_capacity(values.len());
    for value in values {
        let Some(raw) = value.value().as_str() else {
            return Err(ConfigPublicationError::WrongType {
                path: "$.provider.variants[]",
                expected: "a string",
            });
        };
        if raw.is_empty() {
            return Err(ConfigPublicationError::Empty {
                path: "$.provider.variants[]",
            });
        }
        if !seen.insert(raw.to_owned()) {
            return Err(ConfigPublicationError::DuplicateVariant(raw.to_owned()));
        }
        admitted.push(id(ModelVariantId::new(raw), "$.provider.variants[]")?);
    }
    Ok(admitted)
}

pub(super) fn install(
    spec: &CatalogSpec,
    generation: Generation,
) -> Result<(ModelRegistry, ResolvedModelTarget), ConfigPublicationError> {
    let catalog = ModelCatalog::load(catalog_version(generation)?, records(spec)?)
        .ok_or(ConfigPublicationError::CatalogRejected)?;
    let registry = ModelRegistry::install(catalog);
    let target = issue(&registry, spec)?;
    Ok((registry, target))
}

pub(super) fn records(
    spec: &CatalogSpec,
) -> Result<Vec<ModelCatalogRecord>, ConfigPublicationError> {
    if let Some(inventory) = &spec.inventory {
        return inventory.records().map_err(ConfigPublicationError::Load);
    }
    let count = spec.variants.len().max(1);
    require_catalog_rows(count)?;
    let mut rows = Vec::with_capacity(count);
    if spec.variants.is_empty() {
        rows.push(ModelCatalogRecord::new(
            spec.provider().clone(),
            spec.model.clone(),
            spec.base_model.clone(),
            spec.family.clone(),
            None,
        ));
    } else {
        for variant in &spec.variants {
            rows.push(ModelCatalogRecord::new(
                spec.provider().clone(),
                spec.model.clone(),
                spec.base_model.clone(),
                spec.family.clone(),
                Some(variant.clone()),
            ));
        }
    }
    Ok(rows)
}

fn context_window_override(
    spec: &CatalogSpec,
    resolution: &Resolution<'_>,
) -> Result<Option<u64>, ConfigPublicationError> {
    if !spec.accounts().any(|account| {
        account.catalog_provider.as_str() == "openai"
            && account.protocol_family == ProtocolFamily::ChatGptCodex
    }) {
        return Ok(None);
    }
    nearest(resolution, &["model_context_window"])
        .map(|value| {
            value
                .as_number()
                .and_then(|number| number.as_u64())
                .filter(|value| *value > 0)
                .ok_or(ConfigPublicationError::WrongType {
                    path: "$.model_context_window",
                    expected: "a positive integer",
                })
        })
        .transpose()
}

pub(super) fn load_inventory(
    spec: &mut PlatformSpec,
    resolution: &Resolution<'_>,
    root: &Path,
) -> Result<(), ConfigPublicationError> {
    if spec.catalog.provider().as_str() == "deterministic" {
        return Err(ConfigPublicationError::LocalModeRemoved);
    }
    if super::connections::configuration_only(spec, resolution, root)? {
        return Ok(());
    }
    let source = if spec.catalog.managed_connections {
        // Each managed connection supplies its own listing. The provider-list
        // composer merges optional catalog metadata when it is present.
        None
    } else {
        optional_string(resolution, &["catalog", "source"], "$.catalog.source")?
    };
    let Some(source) = source else {
        if spec.catalog.managed_connections
            || matches!(
                spec.catalog.primary.protocol_family,
                ProtocolFamily::OpenAiCompatible
                    | ProtocolFamily::OpenAiResponses
                    | ProtocolFamily::CohereChat
            )
        {
            let accounts = inventory_accounts(&spec.catalog);
            spec.catalog.inventory = Some(Arc::new(
                crate::ModelInventory::load_provider_lists(
                    root,
                    &accounts,
                    &spec.catalog.model,
                    &spec.catalog.family,
                    context_window_override(&spec.catalog, resolution)?,
                )
                .map_err(ConfigPublicationError::Load)?,
            ));
        }
        return Ok(());
    };
    let provider = spec.catalog.primary.catalog_provider.as_str();
    let family = family_spelling(spec.catalog.primary.protocol_family);
    COMPOSED_CATALOGS
        .iter()
        .find(|pair| pair.source == source && pair.provider == provider && pair.family == family)
        .ok_or_else(|| ConfigPublicationError::CatalogComposition {
            catalog_source: source.to_owned(),
            provider_id: provider.to_owned(),
            protocol_family: family.to_owned(),
        })?;
    let accounts = inventory_accounts(&spec.catalog);
    let window = context_window_override(&spec.catalog, resolution)?;
    spec.catalog.inventory = Some(Arc::new(
        crate::ModelInventory::load(root, &accounts, window)
            .map_err(ConfigPublicationError::Load)?,
    ));
    Ok(())
}

fn inventory_accounts(catalog: &CatalogSpec) -> Vec<crate::model_inventory::InventoryAccount> {
    use crate::model_inventory::{DiscoveryShape, InventoryAccount, Metadata};
    catalog
        .accounts()
        .enumerate()
        .map(|(index, account)| InventoryAccount {
            provider: account.provider.clone(),
            catalog_provider: account.catalog_provider.clone(),
            label: account.label.clone(),
            discovery: account.discovery.clone(),
            metadata: if account.protocol_family == ProtocolFamily::ChatGptCodex {
                Metadata::CatalogOrListing
            } else {
                Metadata::Catalog
            },
            shape: match account.protocol_family {
                ProtocolFamily::ChatGptCodex => DiscoveryShape::Codex,
                _ if catalog.managed_connections || index > 0 => {
                    DiscoveryShape::ProviderListOrLegacyCodex
                }
                _ => DiscoveryShape::ProviderList,
            },
        })
        .collect()
}

pub(super) fn require_catalog_rows(actual: usize) -> Result<(), ConfigPublicationError> {
    if actual > MAX_CATALOG_ROWS {
        return Err(ConfigPublicationError::CatalogRows {
            actual,
            bound: MAX_CATALOG_ROWS,
        });
    }
    Ok(())
}

pub(super) fn issue(
    registry: &ModelRegistry,
    spec: &CatalogSpec,
) -> Result<ResolvedModelTarget, ConfigPublicationError> {
    let default = spec
        .selected_variant
        .is_none()
        .then(|| spec.default_variant(spec.provider(), &spec.model))
        .flatten();
    registry
        .issue(
            spec.provider(),
            &spec.model,
            spec.selected_variant.as_ref().or(default.as_ref()),
            spec.routing().clone(),
        )
        .map_err(ConfigPublicationError::Issuance)
}

pub(super) fn catalog_version(
    generation: Generation,
) -> Result<ModelCatalogVersion, ConfigPublicationError> {
    id(
        ModelCatalogVersion::new(format!("platform-config-{}", generation.value())),
        "catalog version",
    )
}

pub(super) fn nearest<'a>(
    resolution: &'a Resolution<'a>,
    path: &[&str],
) -> Option<&'a ConfigValue> {
    resolution
        .contributions_at(path)
        .last()
        .map(|contribution| contribution.value())
}

pub(super) fn required_string(
    resolution: &Resolution<'_>,
    path: &[&str],
    rendered: &'static str,
) -> Result<String, ConfigPublicationError> {
    optional_string(resolution, path, rendered)?
        .ok_or(ConfigPublicationError::Missing { path: rendered })
}

/// Whether `store.authority` is still named. Configurations written before the
/// store chose its own action keep loading; a value no build ever wrote is
/// still refused, so a damaged configuration is not read as current.
pub(super) fn legacy_store_authority(
    resolution: &Resolution<'_>,
) -> Result<bool, ConfigPublicationError> {
    match optional_string(resolution, &["store", "authority"], "$.store.authority")? {
        None => Ok(false),
        Some(value) if matches!(value.as_str(), "initialize" | "migrate" | "open-ready") => {
            Ok(true)
        }
        Some(other) => Err(ConfigPublicationError::StoreDirective(other)),
    }
}

pub(super) fn optional_string(
    resolution: &Resolution<'_>,
    path: &[&str],
    rendered: &'static str,
) -> Result<Option<String>, ConfigPublicationError> {
    let Some(value) = nearest(resolution, path) else {
        return Ok(None);
    };
    let Some(value) = value.as_str() else {
        return Err(ConfigPublicationError::WrongType {
            path: rendered,
            expected: "a string",
        });
    };
    if value.is_empty() {
        return Err(ConfigPublicationError::Empty { path: rendered });
    }
    Ok(Some(value.to_owned()))
}

pub(super) fn id<T>(value: Option<T>, path: &'static str) -> Result<T, ConfigPublicationError> {
    value.ok_or(ConfigPublicationError::Empty { path })
}
