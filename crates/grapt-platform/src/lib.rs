//! Dogfood-0's platform bootstrap boundary.
//!
//! For persistent preparation the order is part of the contract: read-only
//! store classification happens first, complete configuration and credential
//! custody are validated next, and only then may an exact owned-open authority
//! reach SQLite. The product
//! root receives this one typed owner; it never opens a store, invents a model
//! target, or handles provider secret material itself.
//!
//! [`PreparedProviderInputs`] is the store-free alternative for volatile
//! composition: existing configured inputs are read without inspecting SQLite.

mod config;
mod credential;
mod integration;
mod model_inventory;
pub mod profile_configuration;
mod provider_inputs;
mod store;

use std::path::Path;

use grapt_types::{ClockAuthority, IdAuthority, ResolvedModelTarget};

pub use config::{
    composed_catalogs_advice, BuildAttribution, ComposedCatalog, ConfigPublicationError,
    CredentialGrant, ProviderTargetAuthority, PublishedConfig, PublishedReload, StoreDirective,
    COMPOSED_CATALOGS, MAX_CATALOG_ROWS,
};
pub use credential::{
    CredentialAuthority, CredentialCustody, CredentialRefusal, CredentialResolutionRequest,
    CredentialSettlement, ReloadableCredentialAuthority, MAX_CREDENTIAL_BYTES,
};
pub use integration::{
    IntegrationConnection, IntegrationCredentialAuthority, IntegrationCredentialOwner,
    IntegrationCredentialRefusal, IntegrationOAuthWrite, ResolvedIntegrationCredential,
};
pub use model_inventory::{AnthropicThinkingMode, ConnectedModel, ModelInventory};
pub use provider_inputs::{PreparedProviderInputs, ProviderInputParts};
pub use store::{FirstContactKind, PlatformFirstContact, PlatformStoreRefusal};

#[derive(Debug, thiserror::Error)]
pub enum PlatformRefusal {
    #[error("handover build changed before store preparation")]
    HandoverBuildChanged,
    #[error(transparent)]
    Config(#[from] ConfigPublicationError),
    #[error(transparent)]
    Credential(#[from] CredentialRefusal),
    #[error(transparent)]
    Store(#[from] PlatformStoreRefusal),
}

#[allow(
    missing_debug_implementations,
    reason = "the owner holds provider credential custody and must not acquire a formatter"
)]
pub struct PlatformOwner {
    store: grapt_store::OwnedEventStore,
    config: PublishedConfig,
    credential: CredentialCustody,
    /// One held custody per ADDITIONAL configured account, in configuration
    /// order. Each was bound against a target issued for its own provider, so
    /// no secondary account is holding the primary's proof.
    secondary_credentials: Vec<(grapt_types::ProviderId, CredentialCustody)>,
    first_contact: FirstContactKind,
}

/// Platform-owned runtime parts before the provider credential is settled.
///
/// This is the authority/issuance handoff. It does not choose an endpoint,
/// construct an opener, or install a product provider source; the consuming
/// composition lane owns those decisions.
#[allow(
    missing_debug_implementations,
    reason = "provider parts own credential authority and a model-target proof"
)]
pub struct PlatformProviderParts {
    store: grapt_store::EventStore,
    targets: ProviderTargetAuthority,
    build: BuildAttribution,
    credential: CredentialAuthority,
    secondary_credentials: Vec<(grapt_types::ProviderId, CredentialAuthority)>,
    first_contact: FirstContactKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformSettlement {
    first_contact: FirstContactKind,
    build: BuildAttribution,
    credential: CredentialSettlement,
}

impl PlatformProviderParts {
    pub fn into_components(
        self,
    ) -> (
        grapt_store::EventStore,
        ProviderTargetAuthority,
        BuildAttribution,
        CredentialAuthority,
        FirstContactKind,
    ) {
        (
            self.store,
            self.targets,
            self.build,
            self.credential,
            self.first_contact,
        )
    }

    pub fn targets(&self) -> &ProviderTargetAuthority {
        &self.targets
    }

    pub fn credential(&self) -> &CredentialAuthority {
        &self.credential
    }

    /// Take the additional accounts' held custodies. Consuming, like the
    /// primary: a custody that could be taken twice would be a capability that
    /// could be released twice.
    pub fn take_secondary_credentials(
        &mut self,
    ) -> Vec<(grapt_types::ProviderId, CredentialAuthority)> {
        std::mem::take(&mut self.secondary_credentials)
    }
}

impl PlatformSettlement {
    pub fn first_contact(&self) -> FirstContactKind {
        self.first_contact
    }

    pub fn build(&self) -> &BuildAttribution {
        &self.build
    }

    pub fn credential(&self) -> &CredentialSettlement {
        &self.credential
    }
}

/// Bind one held credential custody per ADDITIONAL configured account.
///
/// Each is loaded against a target issued for its OWN provider, because a
/// credential is released only after a live catalog-issued target agrees with
/// every grant coordinate. Binding a second account with the primary's target
/// would be precisely the cross-account substitution that check exists to stop.
fn bind_secondary_credentials(
    service_root: &Path,
    config: &PublishedConfig,
) -> Result<Vec<(grapt_types::ProviderId, CredentialCustody)>, PlatformRefusal> {
    let primary = config.credentials().provider().clone();
    let mut bound = Vec::new();
    for provider in config.configured_providers() {
        if provider == primary {
            continue;
        }
        let Some(grant) = config.credentials_for(&provider) else {
            continue;
        };
        let issuance = config.issue_target_for(&provider)?;
        bound.push((
            provider,
            CredentialCustody::load(service_root, grant, issuance)?,
        ));
    }
    Ok(bound)
}

fn load_primary_credential(
    service_root: &Path,
    config: &PublishedConfig,
    target: ResolvedModelTarget,
) -> Result<CredentialCustody, PlatformRefusal> {
    Ok(CredentialCustody::load(
        service_root,
        config.credentials(),
        target,
    )?)
}

/// Read-only preparation retained until the caller has acquired writer custody.
/// Opening consumes these exact inputs; configuration is not loaded a second time.
#[allow(
    missing_debug_implementations,
    reason = "prepared inputs hold the same unformattable credential custody as PlatformOwner"
)]
pub struct PreparedPlatformInputs {
    first_contact: FirstContactKind,
    store: store::PreparedStoreOpen,
    config: PublishedConfig,
    credential: CredentialCustody,
    secondary_credentials: Vec<(grapt_types::ProviderId, CredentialCustody)>,
}

impl PreparedPlatformInputs {
    pub fn open(self) -> Result<PlatformOwner, PlatformRefusal> {
        let store = self.store.open(self.config.build().as_str())?;
        Ok(PlatformOwner {
            store,
            config: self.config,
            credential: self.credential,
            secondary_credentials: self.secondary_credentials,
            first_contact: self.first_contact,
        })
    }
}

impl PlatformOwner {
    /// First activation of a setup profile consumes the normal store owner
    /// with the pending, already validated connection generation.
    pub fn prepare_pending_connections(
        service_root: &Path,
        records: &grapt_connections::store::Connections,
    ) -> Result<Self, PlatformRefusal> {
        let first_contact = PlatformFirstContact::inspect(service_root);
        let observed = first_contact.kind();
        let PreparedProviderInputs {
            config,
            credential,
            secondary_credentials,
        } = PreparedProviderInputs::prepare_pending_connections(service_root, records)?;
        let store = first_contact.authorize(config.store_directive())?;
        PreparedPlatformInputs {
            first_contact: observed,
            store,
            config,
            credential,
            secondary_credentials,
        }
        .open()
    }

    pub fn prepare(service_root: &Path) -> Result<Self, PlatformRefusal> {
        Self::prepare_for_build(service_root, None)
    }

    /// Check the build pin against the configuration actually used for first
    /// write contact. Ordinary preparation has no handover pin.
    pub fn prepare_for_build(
        service_root: &Path,
        expected_build: Option<&str>,
    ) -> Result<Self, PlatformRefusal> {
        Self::inspect_for_build(service_root, expected_build)?.open()
    }

    pub fn inspect_for_build(
        service_root: &Path,
        expected_build: Option<&str>,
    ) -> Result<PreparedPlatformInputs, PlatformRefusal> {
        let first_contact = PlatformFirstContact::inspect(service_root);
        let PreparedProviderInputs {
            config,
            credential,
            secondary_credentials,
        } = PreparedProviderInputs::load_for_build(service_root, expected_build)?;
        let observed = first_contact.kind();
        let store = first_contact.authorize(config.store_directive())?;
        Ok(PreparedPlatformInputs {
            first_contact: observed,
            store,
            config,
            credential,
            secondary_credentials,
        })
    }

    pub fn prepare_with_authorities(
        service_root: &Path,
        clock: &dyn ClockAuthority,
        ids: &dyn IdAuthority,
    ) -> Result<Self, PlatformRefusal> {
        let first_contact = PlatformFirstContact::inspect(service_root);
        let observed = first_contact.kind();
        let PreparedProviderInputs {
            config,
            credential,
            secondary_credentials,
        } = PreparedProviderInputs::load_for_build(service_root, None)?;
        let store = first_contact.open_with_authorities(
            config.store_directive(),
            config.build().as_str(),
            clock,
            ids,
        )?;
        Ok(Self {
            store,
            config,
            credential,
            secondary_credentials,
            first_contact: observed,
        })
    }

    pub fn first_contact(&self) -> FirstContactKind {
        self.first_contact
    }

    pub fn build(&self) -> &BuildAttribution {
        self.config.build()
    }

    pub fn target(&self) -> Result<&ResolvedModelTarget, ConfigPublicationError> {
        self.config.target()
    }

    pub fn store(&self) -> &grapt_store::EventStore {
        self.store.store()
    }

    pub fn settle(self) -> PlatformSettlement {
        let Self {
            store,
            config,
            credential,
            secondary_credentials,
            first_contact,
        } = self;
        let build = config.build().clone();
        let credential = credential.settle();
        // Every additional account settles too. A held custody that outlived
        // the platform would be a capability with no owner.
        for (_, held) in secondary_credentials {
            drop(held.settle());
        }
        drop(store);
        drop(config);
        PlatformSettlement {
            first_contact,
            build,
            credential,
        }
    }

    /// Transfer the store, live target and one consuming credential authority
    /// without choosing a provider endpoint or runtime composition.
    pub fn into_provider_parts(self) -> Result<PlatformProviderParts, ConfigPublicationError> {
        let Self {
            store,
            config,
            credential,
            secondary_credentials,
            first_contact,
        } = self;
        let ProviderInputParts {
            targets,
            build,
            credential,
            secondary_credentials,
        } = PreparedProviderInputs {
            config,
            credential,
            secondary_credentials,
        }
        .into_components()?;
        Ok(PlatformProviderParts {
            store: store.into_store(),
            targets,
            build,
            credential,
            secondary_credentials,
            first_contact,
        })
    }
}
