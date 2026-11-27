//! Configured provider bootstrap independent of persistent store preparation.
use std::path::Path;

use grapt_types::ProviderId;

use crate::{
    bind_secondary_credentials, load_primary_credential, BuildAttribution, ConfigPublicationError,
    CredentialAuthority, PlatformRefusal, ProviderTargetAuthority, PublishedConfig,
};

/// Existing configured provider inputs prepared without inspecting or opening
/// any database, creating a profile, signing in, or refreshing credentials.
///
/// Configuration, catalog/discovery snapshots and the explicitly configured
/// credential files or environment variables are read using the same bounded
/// loaders and catalog issuance as persistent preparation. Missing or invalid
/// inputs refuse; this API does not discover models or repair account state.
///
/// Captured credentials support one-shot resolution only. Conversion to a
/// reloadable authority refuses with [`crate::CredentialRefusal::RenewalDisabled`]
/// before a consuming product can open a profile-renewal owner. An expired token
/// is not refreshed here and may be rejected by the provider.
///
/// The caller must separately compose its in-memory store and other volatile
/// services before any registration, logging, artifact or session side effects.
/// This boundary alone is not an ephemeral-session guarantee. It cannot control
/// provider retention, OS access metadata, swap, crash dumps or auditing. On
/// Windows, protected credential inputs use the existing pipe-fed DPAPI helper.
#[allow(
    missing_debug_implementations,
    reason = "prepared provider inputs hold unformattable credential authority"
)]
pub struct PreparedProviderInputs {
    pub(super) config: PublishedConfig,
    pub(super) credential: CredentialAuthority,
    pub(super) secondary_credentials: Vec<(ProviderId, CredentialAuthority)>,
}

/// Store-free provider authorities and configuration metadata for composition.
#[allow(
    missing_debug_implementations,
    reason = "provider input parts own credential authority and must not acquire a formatter"
)]
pub struct ProviderInputParts {
    pub targets: ProviderTargetAuthority,
    pub build: BuildAttribution,
    pub credential: CredentialAuthority,
    pub secondary_credentials: Vec<(ProviderId, CredentialAuthority)>,
}

impl PreparedProviderInputs {
    /// Build the prospective generation while its owning connection store
    /// holds the synchronous install lock. Reads staged named vault files,
    /// never grants database authority, and does not publish the metadata.
    pub fn prepare_pending_connections(
        service_root: &Path,
        records: &grapt_connections::store::Connections,
    ) -> Result<Self, PlatformRefusal> {
        let config = PublishedConfig::load_pending_connections(service_root, records)?;
        Self::from_config(service_root, config)
    }
    pub fn prepare(service_root: &Path) -> Result<Self, PlatformRefusal> {
        Self::prepare_for_build(service_root, None)
    }

    /// Check an optional handover build pin against the inputs actually used.
    /// The configured store directive is validated as configuration but never
    /// grants database inspection, initialization, migration or open authority.
    pub fn prepare_for_build(
        service_root: &Path,
        expected_build: Option<&str>,
    ) -> Result<Self, PlatformRefusal> {
        let mut inputs = Self::load_for_build(service_root, expected_build)?;
        inputs.credential.disable_renewal();
        for (_, credential) in &mut inputs.secondary_credentials {
            credential.disable_renewal();
        }
        Ok(inputs)
    }

    // Shared with persistent preparation, which retains its existing renewal
    // capability and performs first-contact classification before calling here.
    pub(super) fn load_for_build(
        service_root: &Path,
        expected_build: Option<&str>,
    ) -> Result<Self, PlatformRefusal> {
        let config = PublishedConfig::load_for_build(service_root, expected_build)?;
        Self::from_config(service_root, config)
    }

    fn from_config(service_root: &Path, config: PublishedConfig) -> Result<Self, PlatformRefusal> {
        let issuance = config.issue_target()?;
        let credential = load_primary_credential(service_root, &config, issuance)?;
        let secondary_credentials = bind_secondary_credentials(service_root, &config)?;
        Ok(Self {
            config,
            credential,
            secondary_credentials,
        })
    }

    /// Transfer the existing authorities and metadata without creating a store.
    /// Secondary credentials retain configuration order and provider identity.
    ///
    /// Use `targets.issue()` (or its selection API) and the credential's record,
    /// protocol family and placement to form a `CredentialResolutionRequest`,
    /// then consume it with `credential.resolve(request)`. Do not use the
    /// persistent product's reloadable/profile-renewal composition path.
    pub fn into_components(self) -> Result<ProviderInputParts, ConfigPublicationError> {
        let build = self.config.build().clone();
        let targets = self.config.into_target_authority()?;
        Ok(ProviderInputParts {
            targets,
            build,
            credential: self.credential,
            secondary_credentials: self.secondary_credentials,
        })
    }
}
