use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use grapt_config::{
    ConfigBounds, ConfigFileName, ConfigSession, ConfigSourcePath, ConfigValue, Generation,
    MaxAncestorDepth, MaxDocumentBytes, MaxDocumentNodes, MaxDocuments, MaxPathBytes,
    MaxValueDepth, Resolution, SourceBounds, SourceReader,
};
use grapt_provider::{CredentialPlacement, ProtocolFamily};
use grapt_types::{
    AccountId, BaseModelId, CredentialRecordId, CredentialStorageMode, CredentialVersion,
    ModelCatalog, ModelCatalogRecord, ModelCatalogVersion, ModelFamilyId, ModelId, ModelRegistry,
    ModelRouteId, ModelVariantId, PolicyVersion, ProviderId, ResolvedModelTarget, RoutingRequest,
};

pub const MAX_CATALOG_ROWS: usize = 16;
/// Additional connections beyond the primary one. Bounded like every other
/// startup input; the inventory applies its own account bound independently.
const MAX_SECONDARY_ACCOUNTS: usize = 7;
/// The established discovery input for the primary connection.
const PRIMARY_DISCOVERY_FILE: &str = "account-models.json";
const LOCAL_DOGFOOD_CREDENTIAL_RECORD: &str = "cred_local_dogfood_provider_file_v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildAttribution {
    build: String,
    generation: Generation,
}

impl BuildAttribution {
    pub fn as_str(&self) -> &str {
        &self.build
    }

    pub fn generation(&self) -> Generation {
        self.generation
    }
}

/// The exact non-secret grant bound to the local dogfood credential source.
///
/// Fields and construction stay private to the platform configuration owner.
/// A downstream caller may read inert coordinates needed to request resolution,
/// but it cannot manufacture a grant from authored strings.
#[allow(
    missing_debug_implementations,
    reason = "an authority grant has no formatter; PublishedConfig prints a fixed redaction"
)]
pub struct CredentialGrant {
    record: CredentialRecordId,
    provider: ProviderId,
    protocol_family: ProtocolFamily,
    route: ModelRouteId,
    account: AccountId,
    version: CredentialVersion,
    placement: CredentialPlacement,
    storage: CredentialStorageMode,
    source: crate::credential::CredentialSourceName,
    credential_format: grapt_connections::store::CredentialFormat,
    /// The environment variable a `ProcessEnvironment` grant reads (J01): a
    /// name only; the value is read when the credential loads.
    environment: Option<String>,
    chatgpt_files: Option<grapt_login::chatgpt::SignInFiles>,
}

impl CredentialGrant {
    pub fn record(&self) -> &CredentialRecordId {
        &self.record
    }

    pub fn provider(&self) -> &ProviderId {
        &self.provider
    }

    pub fn protocol_family(&self) -> ProtocolFamily {
        self.protocol_family
    }

    pub fn route(&self) -> &ModelRouteId {
        &self.route
    }

    pub fn account(&self) -> &AccountId {
        &self.account
    }

    pub fn version(&self) -> &CredentialVersion {
        &self.version
    }

    pub fn placement(&self) -> CredentialPlacement {
        self.placement
    }

    pub fn storage(&self) -> CredentialStorageMode {
        self.storage
    }

    /// Which bounded local input this grant's secret is read from. Crate-private
    /// so a downstream caller cannot redirect a grant at another file.
    pub(crate) fn source(&self) -> &crate::credential::CredentialSourceName {
        &self.source
    }

    pub(crate) fn credential_format(&self) -> &grapt_connections::store::CredentialFormat {
        &self.credential_format
    }

    /// The variable a `ProcessEnvironment` grant reads, by name.
    pub(crate) fn environment(&self) -> Option<&str> {
        self.environment.as_deref()
    }

    pub(crate) fn chatgpt_files(&self) -> Option<&grapt_login::chatgpt::SignInFiles> {
        self.chatgpt_files.as_ref()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigPublicationError {
    #[error("Local script mode has been removed. Start grapt code --profile <new-profile> and choose Connect (/connect) to configure a provider.")]
    LocalModeRemoved,
    #[error("platform configuration bounds are invalid: {0}")]
    Bounds(String),
    #[error("platform configuration path is invalid: {0}")]
    Path(String),
    #[error("platform configuration could not be loaded: {0}")]
    Load(String),
    #[error(
        "catalog source {catalog_source:?} is not composed with provider id {provider_id:?} \
         and protocol family {protocol_family}; {}",
        composed_catalogs_advice()
    )]
    CatalogComposition {
        catalog_source: String,
        provider_id: String,
        protocol_family: String,
    },
    /// The configured value is never echoed: a key pasted here by mistake must
    /// not reach a terminal or log.
    #[error(
        "$.provider.credential must be env:NAME naming an environment variable \
         (letters A-Z, digits and _); the primary's stored key belongs in the \
         provider.credential file, not in configuration"
    )]
    CredentialReference,
    #[error("platform configuration field {path} is missing")]
    Missing { path: &'static str },
    #[error("platform configuration field {path} must be {expected}")]
    WrongType {
        path: &'static str,
        expected: &'static str,
    },
    #[error("platform configuration field {path} is empty")]
    Empty { path: &'static str },
    #[error("platform store authority {0:?} is not recognized")]
    StoreDirective(String),
    #[error("platform catalog expands to {actual} rows, above the bound {bound}")]
    CatalogRows { actual: usize, bound: usize },
    #[error("platform catalog repeats variant {0:?}")]
    DuplicateVariant(String),
    #[error("selected variant {0:?} is absent from the bounded catalog rows")]
    SelectedVariantAbsent(String),
    #[error("platform catalog release contradicted itself")]
    CatalogRejected,
    #[error("platform target issuance was refused: {0:?}")]
    Issuance(grapt_types::IssuanceRefusal),
    #[error("platform configuration reload was refused: {0}")]
    Reload(String),
    #[error("platform target state was unavailable during publication")]
    TargetState,
    #[error("the fixed local dogfood credential record identity is invalid")]
    LocalCredentialRecord,
    #[error("configuration declares {actual} additional accounts, above the bound {bound}")]
    AccountCount { actual: usize, bound: usize },
    #[error(
        "configuration declares provider {provider:?} or one of its inputs twice: \
         {entry} and {existing} share the {input}; each account must differ from \
         the primary and every other account in provider id, credential source \
         and discovery file"
    )]
    DuplicateAccount {
        provider: String,
        entry: String,
        existing: String,
        input: &'static str,
    },
    #[error("configured protocol family {0:?} is not recognized")]
    ProtocolFamily(String),
    #[error("selection named provider {0:?}, which has no configured account")]
    UnconfiguredProvider(String),
    #[error("account {0:?} speaks a protocol with no default endpoint and configures none")]
    EndpointRequired(String),
}

/// A catalog source the platform composes: the provider id and protocol
/// family it serves, and the shape of that account's discovery snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComposedCatalog {
    pub provider: &'static str,
    pub source: &'static str,
    pub family: &'static str,
    pub(crate) shape: crate::model_inventory::DiscoveryShape,
}

/// Every composed catalog pair (J01): the single source for composition and,
/// through [`composed_catalogs_advice`], for its refusal text.
pub const COMPOSED_CATALOGS: [ComposedCatalog; 3] = [
    ComposedCatalog {
        provider: "openai",
        source: "models.dev-chatgpt",
        family: "chatgpt-codex",
        shape: crate::model_inventory::DiscoveryShape::Codex,
    },
    ComposedCatalog {
        provider: "anthropic",
        source: "models.dev-anthropic",
        family: "anthropic-messages",
        shape: crate::model_inventory::DiscoveryShape::ProviderList,
    },
    ComposedCatalog {
        provider: "openai",
        source: "models.dev-openai",
        family: "openai-responses",
        shape: crate::model_inventory::DiscoveryShape::ProviderList,
    },
];

/// What a composition refusal advises: every [`COMPOSED_CATALOGS`] row, read
/// from the table itself so the advice cannot drift from what composes. It
/// quotes no configured value, so the terminal can show it; its length is
/// bounded by the table's fixed size.
pub fn composed_catalogs_advice() -> String {
    let pairs: Vec<String> = COMPOSED_CATALOGS
        .iter()
        .map(|pair| {
            format!(
                "provider id {:?} with catalog source {:?} (protocol family {})",
                pair.provider, pair.source, pair.family
            )
        })
        .collect();
    format!(
        "composed pairs: {}; use one of those, or remove catalog.source and declare your models in provider.variants",
        pairs.join(", ")
    )
}

/// One configured provider connection: who it reaches, how it authenticates,
/// and which bounded local inputs carry its credential and its discovery.
///
/// The primary account is held as its own field rather than as element zero of
/// a list, so "there is always a default connection" is structural and carries
/// no indexing panic. It is derived from the established top-level
/// `$.provider.*` fields, so a single-account configuration composes to exactly
/// one of these and keeps its prior behavior.
struct AccountSpec {
    watsonx_project: Option<grapt_types::WatsonxProjectId>,
    chatgpt_files: Option<grapt_login::chatgpt::SignInFiles>,
    provider: ProviderId,
    catalog_provider: ProviderId,
    label: Option<String>,
    routing: RoutingRequest,
    protocol_family: ProtocolFamily,
    credential_source: crate::credential::CredentialSourceName,
    credential_format: grapt_connections::store::CredentialFormat,
    /// `Some(NAME)` when the credential is the user's exported variable NAME
    /// (`provider.credential: "env:NAME"`, J01); the profile holds only the
    /// name.
    credential_environment: Option<String>,
    discovery: String,
    /// The endpoint base this account is reached at.
    ///
    /// `None` means the protocol family's own default is used. A family that
    /// has no default — `OpenAiCompatible`, which is every aggregator and
    /// self-hosted endpoint — therefore REQUIRES one, and composition refuses
    /// without it rather than resolving to an empty base at attempt time.
    endpoint: Option<String>,
}

struct CatalogSpec {
    managed_connections: bool,
    model: ModelId,
    base_model: BaseModelId,
    family: ModelFamilyId,
    variants: Vec<ModelVariantId>,
    selected_variant: Option<ModelVariantId>,
    primary: AccountSpec,
    secondary: Vec<AccountSpec>,
    inventory: Option<Arc<crate::ModelInventory>>,
}

impl CatalogSpec {
    fn provider(&self) -> &ProviderId {
        &self.primary.provider
    }

    /// The inventory's default reasoning level for a model, used when a
    /// selection names none.
    fn default_variant(&self, provider: &ProviderId, model: &ModelId) -> Option<ModelVariantId> {
        if provider == self.provider() && model == &self.model {
            if let Some(selected) = &self.selected_variant {
                return Some(selected.clone());
            }
        }
        self.inventory
            .as_ref()?
            .available()
            .iter()
            .find(|row| row.provider() == provider && row.id() == model)?
            .default_variant()
            .cloned()
    }

    fn routing(&self) -> &RoutingRequest {
        &self.primary.routing
    }

    fn accounts(&self) -> impl Iterator<Item = &AccountSpec> {
        std::iter::once(&self.primary).chain(self.secondary.iter())
    }

    /// The connection a provider is reachable through, or `None` when that
    /// provider has no configured account. `None` is a refusal coordinate: no
    /// caller may substitute the default connection for a missing one, which is
    /// the "no silent route/model/account fallback" invariant.
    fn account_for(&self, provider: &ProviderId) -> Option<&AccountSpec> {
        self.accounts()
            .find(|account| &account.provider == provider)
    }
}

/// The platform's live authority to issue one fresh linear target per physical
/// attempt from the one loaded catalog lineage.
///
/// It is deliberately neither `Clone` nor `Debug`: cloning would duplicate the
/// registry authority, while formatting could reopen the catalog facts that
/// [`ResolvedModelTarget`] keeps sealed.
#[allow(
    missing_debug_implementations,
    reason = "the authority owns a live registry and sealed catalog facts"
)]
pub struct ProviderTargetAuthority {
    registry: ModelRegistry,
    catalog: CatalogSpec,
    protocol_family: ProtocolFamily,
}

impl ProviderTargetAuthority {
    /// The catalog effort inherited by the primary model when a session names
    /// none. This is a read-only fact; it issues no target or selection.
    pub fn default_effort(&self) -> Option<ModelVariantId> {
        self.catalog
            .default_variant(self.catalog.provider(), &self.catalog.model)
    }

    /// Non-secret account labels from the bounded, published connection catalog.
    pub fn accounts(&self) -> impl Iterator<Item = (&ProviderId, &AccountId)> {
        self.catalog
            .accounts()
            .map(|account| (&account.provider, &account.routing.account))
    }

    /// Non-secret environment names read by this loaded provider catalog.
    /// Child-process adapters must omit these even if a config file is edited.
    pub fn credential_environment_names(&self) -> impl Iterator<Item = &str> {
        self.catalog
            .accounts()
            .filter_map(|account| account.credential_environment.as_deref())
    }

    pub fn inventory(&self) -> Option<Arc<crate::ModelInventory>> {
        self.catalog.inventory.as_ref().map(Arc::clone)
    }

    /// Selection chooses a row from the already published catalog. It cannot
    /// manufacture provider, account, route or credential authority.
    ///
    /// The selected provider names WHICH configured connection issues the
    /// target, so its route, account, credential version and policy come from
    /// that account. A provider with no configured account is refused by name;
    /// it never falls back to the primary connection, because serving one
    /// provider's selection through another account is precisely the silent
    /// cross-model swap the attempt law forbids.
    pub fn issue_selected(
        &self,
        provider: &ProviderId,
        model: &ModelId,
        variant: Option<&ModelVariantId>,
    ) -> Result<ResolvedModelTarget, ConfigPublicationError> {
        let account = self.catalog.account_for(provider).ok_or_else(|| {
            ConfigPublicationError::UnconfiguredProvider(provider.as_str().to_owned())
        })?;
        let default = variant
            .is_none()
            .then(|| self.catalog.default_variant(provider, model))
            .flatten();
        self.registry
            .issue(
                provider,
                model,
                variant.or(default.as_ref()),
                account.routing.clone(),
            )
            .map_err(ConfigPublicationError::Issuance)
    }
    pub fn provider(&self) -> &ProviderId {
        self.catalog.provider()
    }

    pub fn model(&self) -> &ModelId {
        &self.catalog.model
    }

    /// The protocol family of the primary connection. Use
    /// [`protocol_family_for`](Self::protocol_family_for) when a selection has
    /// already named a provider.
    pub fn protocol_family(&self) -> ProtocolFamily {
        self.protocol_family
    }

    /// The protocol family the named provider is actually reached over, or
    /// `None` when it has no configured account.
    pub fn protocol_family_for(&self, provider: &ProviderId) -> Option<ProtocolFamily> {
        self.catalog
            .account_for(provider)
            .map(|account| account.protocol_family)
    }

    /// The IBM project retained by this connection's published generation.
    pub fn watsonx_project_for(
        &self,
        provider: &ProviderId,
    ) -> Option<&grapt_types::WatsonxProjectId> {
        self.catalog
            .account_for(provider)
            .and_then(|account| account.watsonx_project.as_ref())
    }

    /// The configured endpoint base; absent accounts and family defaults return None.
    pub fn endpoint_for(&self, provider: &ProviderId) -> Option<&str> {
        self.catalog
            .account_for(provider)
            .and_then(|account| account.endpoint.as_deref())
    }
    pub fn cloudflare_for(
        &self,
        provider: &ProviderId,
    ) -> Option<&grapt_types::cloudflare::CloudflareGatewayScope> {
        self.catalog
            .account_for(provider)
            .and_then(|account| account.credential_format.cloudflare())
    }

    pub fn messages_auth_for(&self, provider: &ProviderId) -> grapt_provider::MessagesAuthMode {
        match self
            .catalog
            .account_for(provider)
            .map(|account| &account.credential_format)
        {
            Some(grapt_connections::store::CredentialFormat::MessagesBearer { .. }) => {
                grapt_provider::MessagesAuthMode::Bearer
            }
            _ => grapt_provider::MessagesAuthMode::ApiKey,
        }
    }

    pub fn compatible_reasoning_replay_for(
        &self,
        provider: &ProviderId,
    ) -> grapt_provider::CompatibleReasoningReplay {
        if self.catalog.account_for(provider).is_some_and(|account| {
            account.protocol_family == ProtocolFamily::OpenAiCompatible
                && grapt_connections::catalog::requires_reasoning_content_replay(
                    &account.catalog_provider,
                )
        }) {
            grapt_provider::CompatibleReasoningReplay::ReasoningContent
        } else {
            grapt_provider::CompatibleReasoningReplay::None
        }
    }

    /// Catalogue identity survives credential aliases and endpoint overrides.
    pub fn compatible_protocol_for(
        &self,
        provider: &ProviderId,
    ) -> Option<grapt_provider::CompatibleProtocol> {
        self.catalog
            .account_for(provider)
            .map(|account| match account.catalog_provider.as_str() {
                "groq" => grapt_provider::CompatibleProtocol::Groq,
                id if grapt_connections::catalog::endpoint_provider(id)
                    .is_some_and(|row| row.completion_tokens) =>
                {
                    grapt_provider::CompatibleProtocol::CompletionTokens
                }
                _ => grapt_provider::CompatibleProtocol::OpenAi,
            })
    }

    pub fn route(&self) -> &ModelRouteId {
        &self.catalog.routing().route
    }

    /// Mint a fresh, non-cloneable target for one physical attempt.
    pub fn issue(&self) -> Result<ResolvedModelTarget, ConfigPublicationError> {
        issue(&self.registry, &self.catalog)
    }
}

struct PlatformSpec {
    /// Whether the configuration still names `store.authority`, which the
    /// store no longer reads.
    legacy_store_authority: bool,
    build: String,
    catalog: CatalogSpec,
}

enum TargetState {
    Current(Box<ResolvedModelTarget>),
    Publishing,
}

pub struct PublishedConfig {
    service_root: std::path::PathBuf,
    session: ConfigSession,
    registry: ModelRegistry,
    catalog: CatalogSpec,
    target: TargetState,
    legacy_store_authority: bool,
    build: BuildAttribution,
    credentials: CredentialGrants,
}

/// Every configured connection's non-secret grant. The primary is its own field
/// for the same reason it is in [`CatalogSpec`]: "there is always a default
/// connection" stays structural and carries no indexing panic.
#[allow(
    missing_debug_implementations,
    reason = "grants have no formatter; PublishedConfig prints a fixed redaction"
)]
pub struct CredentialGrants {
    primary: CredentialGrant,
    secondary: Vec<CredentialGrant>,
}

impl CredentialGrants {
    fn iter(&self) -> impl Iterator<Item = &CredentialGrant> {
        std::iter::once(&self.primary).chain(self.secondary.iter())
    }
}

impl core::fmt::Debug for PublishedConfig {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("PublishedConfig")
            .field("generation", &self.session.generation())
            .field("legacy_store_authority", &self.legacy_store_authority)
            .field("build", &self.build)
            .field("credentials", &"<credential-grants>")
            .field("target", &"<catalog-issued>")
            .finish()
    }
}

pub struct PublishedReload {
    retired_target: ResolvedModelTarget,
    from: Generation,
    to: Generation,
}

impl core::fmt::Debug for PublishedReload {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("PublishedReload")
            .field("retired_target", &"<catalog-issued>")
            .field("from", &self.from)
            .field("to", &self.to)
            .finish()
    }
}

impl PublishedReload {
    pub fn from(&self) -> Generation {
        self.from
    }

    pub fn to(&self) -> Generation {
        self.to
    }

    pub fn retired_target(&self) -> &ResolvedModelTarget {
        &self.retired_target
    }
}

impl PublishedConfig {
    /// An explicit empty managed generation has no provider authority. Read
    /// only its normal resolved build/store header so setup can serve before
    /// the first credential exists. Corrupt input never becomes setup mode.
    pub fn empty_profile_build(
        service_root: &Path,
        expected: Option<&str>,
    ) -> Result<Option<BuildAttribution>, crate::PlatformRefusal> {
        if !connections::read(service_root)?.is_some_and(|records| records.connections.is_empty()) {
            return Ok(None);
        }
        let (plan, bounds, source_bounds) = config_inputs(service_root)?;
        let session = ConfigSession::load(
            plan,
            &crate::profile_configuration::ProfileSourceReader,
            bounds,
            source_bounds,
        )
        .map_err(|error| ConfigPublicationError::Load(error.to_string()))?;
        let resolution = grapt_config::resolve(session.configuration());
        derive::legacy_store_authority(&resolution)?;
        let build = derive::required_string(&resolution, &["product", "build"], "$.product.build")?;
        if expected.is_some_and(|expected| expected != build) {
            return Err(crate::PlatformRefusal::HandoverBuildChanged);
        }
        Ok(Some(BuildAttribution {
            build,
            generation: session.generation(),
        }))
    }

    /// Check a retained handover pin before any credential renewal or write.
    pub fn load_for_build(
        service_root: &Path,
        expected: Option<&str>,
    ) -> Result<Self, crate::PlatformRefusal> {
        let config = Self::load(service_root)?;
        config.check_build(expected)?;
        Ok(config)
    }

    pub fn check_build(&self, expected: Option<&str>) -> Result<(), crate::PlatformRefusal> {
        if expected.is_some_and(|expected| expected != self.build().as_str()) {
            return Err(crate::PlatformRefusal::HandoverBuildChanged);
        }
        Ok(())
    }

    pub fn load(service_root: &Path) -> Result<Self, ConfigPublicationError> {
        Self::load_with_reader(
            service_root,
            &crate::profile_configuration::ProfileSourceReader,
        )
    }

    pub fn load_with_reader(
        service_root: &Path,
        reader: &dyn SourceReader,
    ) -> Result<Self, ConfigPublicationError> {
        Self::load_with_connections(service_root, reader, None)
    }

    pub(crate) fn load_pending_connections(
        service_root: &Path,
        records: &grapt_connections::store::Connections,
    ) -> Result<Self, ConfigPublicationError> {
        records
            .to_vec()
            .map_err(|_| ConfigPublicationError::Load("proposed connections are invalid".into()))?;
        Self::load_with_connections(
            service_root,
            &crate::profile_configuration::ProfileSourceReader,
            Some(records.clone()),
        )
    }

    fn load_with_connections(
        service_root: &Path,
        reader: &dyn SourceReader,
        records: Option<grapt_connections::store::Connections>,
    ) -> Result<Self, ConfigPublicationError> {
        let (plan, bounds, source_bounds) = config_inputs(service_root)?;
        let session = ConfigSession::load(plan, reader, bounds, source_bounds)
            .map_err(|error| ConfigPublicationError::Load(error.to_string()))?;
        let generation = session.generation();
        let resolution = grapt_config::resolve(session.configuration());
        let mut spec = derive_spec_with_connections(&resolution, service_root, records)?;
        load_inventory(&mut spec, &resolution, service_root)?;
        let (registry, target) = install(&spec.catalog, generation)?;
        let credentials = grants(&spec.catalog)?;
        Ok(Self {
            service_root: service_root.to_path_buf(),
            session,
            registry,
            catalog: spec.catalog,
            target: TargetState::Current(Box::new(target)),
            legacy_store_authority: spec.legacy_store_authority,
            build: BuildAttribution {
                build: spec.build,
                generation,
            },
            credentials,
        })
    }

    pub fn generation(&self) -> Generation {
        self.session.generation()
    }

    /// Whether the configuration still names `store.authority`. The store is
    /// created, migrated or opened as it is found, so the key is ignored.
    pub fn names_legacy_store_authority(&self) -> bool {
        self.legacy_store_authority
    }

    pub fn build(&self) -> &BuildAttribution {
        &self.build
    }

    /// The primary connection's grant. Preserved as the single-account read
    /// path; a caller that already knows which provider it selected asks
    /// [`credentials_for`](Self::credentials_for) instead.
    pub fn credentials(&self) -> &CredentialGrant {
        &self.credentials.primary
    }

    /// The grant for one configured provider, or `None` when that provider has
    /// no configured account. Absence is a refusal coordinate, never a cue to
    /// authenticate a second provider with the primary account's credential.
    /// The composed catalog inventory, when this configuration names a catalog
    /// source. Read-only breadth plus the connected set; it issues nothing.
    pub fn inventory(&self) -> Option<Arc<crate::ModelInventory>> {
        self.catalog.inventory.as_ref().map(Arc::clone)
    }

    pub fn credentials_for(&self, provider: &ProviderId) -> Option<&CredentialGrant> {
        self.credentials
            .iter()
            .find(|grant| grant.provider() == provider)
    }

    pub fn target(&self) -> Result<&ResolvedModelTarget, ConfigPublicationError> {
        match &self.target {
            TargetState::Current(target) => Ok(target.as_ref()),
            TargetState::Publishing => Err(ConfigPublicationError::TargetState),
        }
    }

    /// Mint another target from the current publication without duplicating a
    /// prior target proof.
    pub fn issue_target(&self) -> Result<ResolvedModelTarget, ConfigPublicationError> {
        issue(&self.registry, &self.catalog)
    }

    /// Mint a target for one configured provider, for binding that account's
    /// own credential.
    ///
    /// A credential is released only against a live catalog-issued target that
    /// agrees with every grant coordinate, so a second account cannot be bound
    /// with the primary account's target. The model chosen is that provider's
    /// first discovered row: which row it is does not matter to the binding,
    /// only that the target genuinely belongs to this account.
    pub fn issue_target_for(
        &self,
        provider: &ProviderId,
    ) -> Result<ResolvedModelTarget, ConfigPublicationError> {
        let account = self.catalog.account_for(provider).ok_or_else(|| {
            ConfigPublicationError::UnconfiguredProvider(provider.as_str().to_owned())
        })?;
        let model = self
            .catalog
            .inventory
            .as_ref()
            .and_then(|inventory| {
                inventory
                    .available()
                    .iter()
                    .find(|model| model.provider() == provider)
                    .map(|model| model.id().clone())
            })
            .unwrap_or_else(|| self.catalog.model.clone());
        let variant = self.catalog.default_variant(provider, &model);
        self.registry
            .issue(provider, &model, variant.as_ref(), account.routing.clone())
            .map_err(ConfigPublicationError::Issuance)
    }

    /// Every configured provider, primary first. The caller binds one
    /// credential per entry.
    pub fn configured_providers(&self) -> Vec<ProviderId> {
        self.catalog
            .accounts()
            .map(|account| account.provider.clone())
            .collect()
    }

    /// The approved file pairs/account coordinates for startup renewal.
    pub fn chatgpt_sign_ins(
        &self,
    ) -> impl Iterator<Item = (&CredentialGrant, &grapt_login::chatgpt::SignInFiles)> {
        self.credentials
            .iter()
            .filter_map(|grant| grant.chatgpt_files().map(|files| (grant, files)))
    }

    pub(crate) fn into_target_authority(
        self,
    ) -> Result<ProviderTargetAuthority, ConfigPublicationError> {
        match self.target {
            TargetState::Current(target) => drop(target),
            TargetState::Publishing => return Err(ConfigPublicationError::TargetState),
        }
        let protocol_family = self.catalog.primary.protocol_family;
        Ok(ProviderTargetAuthority {
            registry: self.registry,
            catalog: self.catalog,
            protocol_family,
        })
    }

    pub fn reload(
        mut self,
        reader: &dyn SourceReader,
    ) -> Result<(Self, PublishedReload), ConfigPublicationError> {
        let from = self.session.generation();
        let to = self
            .session
            .reload(reader)
            .map_err(|error| ConfigPublicationError::Reload(error.to_string()))?;
        let resolution = grapt_config::resolve(self.session.configuration());
        let mut spec = derive_spec(&resolution, &self.service_root)?;
        load_inventory(&mut spec, &resolution, &self.service_root)?;
        let version = catalog_version(to)?;
        let records = records(&spec.catalog)?;
        let retired_catalog = self
            .registry
            .supersede(version, records)
            .ok_or(ConfigPublicationError::CatalogRejected)?;
        let target = issue(&self.registry, &spec.catalog)?;
        let retired_target = match std::mem::replace(&mut self.target, TargetState::Publishing) {
            TargetState::Current(target) => *target,
            TargetState::Publishing => return Err(ConfigPublicationError::TargetState),
        };
        self.target = TargetState::Current(Box::new(target));
        self.credentials = grants(&spec.catalog)?;
        self.catalog = spec.catalog;
        self.legacy_store_authority = spec.legacy_store_authority;
        self.build = BuildAttribution {
            build: spec.build,
            generation: to,
        };
        drop(retired_catalog);
        Ok((
            self,
            PublishedReload {
                retired_target,
                from,
                to,
            },
        ))
    }
}

mod derive;
use derive::*;
mod connections;
