//! Re-read only the local input sealed by the original credential grant.
use std::path::PathBuf;

use grapt_provider::{CredentialRevision, CurrentCredential};

use super::*;

#[cfg(test)]
mod tests;

pub(super) struct BoundSource {
    root: PathBuf,
    connection_use: Option<std::sync::Arc<super::connection_use::ConnectionUse>>,
    vault: Option<super::vault::VaultSource>,
    adc: Option<std::sync::Arc<grapt_login::vertex::adc::Session>>,
    aws: Option<grapt_login::aws::Selection>,
    storage: CredentialStorageMode,
    name: CredentialSourceName,
    environment: Option<String>,
    root_identity: Option<same_file::Handle>,
    chatgpt_files: Option<grapt_login::chatgpt::SignInFiles>,
}

impl BoundSource {
    pub(super) fn is_vaulted_api_key(&self) -> bool {
        self.vault
            .as_ref()
            .is_some_and(super::vault::VaultSource::is_static_key)
            && self.chatgpt_files.is_none()
    }

    pub(super) fn new(root: &Path, grant: &CredentialGrant) -> Result<Self, CredentialRefusal> {
        if matches!(
            grant.protocol_family(),
            ProtocolFamily::VertexGemini
                | ProtocolFamily::VertexAnthropic
                | ProtocolFamily::WatsonxChat
        ) && (grant.credential_format().binding().is_none()
            || grant.chatgpt_files().is_some()
            || grant.environment().is_some())
        {
            return Err(CredentialRefusal::GrantStorageMismatch);
        }
        let (root, root_identity) = if matches!(
            grant.storage(),
            CredentialStorageMode::PlaintextFile | CredentialStorageMode::EncryptedFile
        ) {
            if !root.is_absolute() {
                return Err(CredentialRefusal::RootNotAbsolute);
            }
            let metadata =
                std::fs::symlink_metadata(root).map_err(CredentialRefusal::RootMetadata)?;
            if link_or_reparse(root, &metadata)? {
                return Err(CredentialRefusal::RootLinked);
            }
            if !metadata.is_dir() {
                return Err(CredentialRefusal::RootNotDirectory);
            }
            let canonical = std::fs::canonicalize(root).map_err(CredentialRefusal::RootResolve)?;
            let identity = same_file::Handle::from_path(&canonical)
                .map_err(CredentialRefusal::RootMetadata)?;
            (canonical, Some(identity))
        } else {
            (root.to_owned(), None)
        };
        let vault = super::vault::VaultSource::new(grant)?;
        let adc = if let Some(vault) = &vault {
            if vault.is_vertex_adc() {
                vault.check_adc(&root)?;
                Some(std::sync::Arc::new(
                    grapt_login::vertex::adc::Session::open()
                        .map_err(CredentialRefusal::GoogleAdc)?,
                ))
            } else {
                None
            }
        } else {
            None
        };
        let aws = vault
            .as_ref()
            .map(|vault| vault.aws_selection(&root))
            .transpose()?
            .flatten();
        Ok(Self {
            adc,
            aws,
            connection_use: super::connection_use::ConnectionUse::from_grant(&root, grant)?,
            vault,
            root,
            storage: grant.storage(),
            name: grant.source().clone(),
            environment: grant.environment().map(str::to_owned),
            root_identity,
            chatgpt_files: grant.chatgpt_files().cloned(),
        })
    }

    fn validate_root(&self) -> Result<(), CredentialRefusal> {
        if let Some(identity) = &self.root_identity {
            let metadata =
                std::fs::symlink_metadata(&self.root).map_err(CredentialRefusal::RootMetadata)?;
            if link_or_reparse(&self.root, &metadata)? {
                return Err(CredentialRefusal::RootLinked);
            }
            if !metadata.is_dir() {
                return Err(CredentialRefusal::RootNotDirectory);
            }
            let current = same_file::Handle::from_path(&self.root)
                .map_err(CredentialRefusal::RootMetadata)?;
            if &current != identity {
                return Err(CredentialRefusal::RootChanged);
            }
        }
        Ok(())
    }

    pub(super) fn bind(
        &self,
        credential: OpenerCredential,
    ) -> Result<OpenerCredential, CredentialRefusal> {
        match &self.vault {
            Some(vault) => vault.bind(credential),
            None => Ok(credential),
        }
    }

    pub(super) fn bind_use(&self, credential: OpenerCredential) -> OpenerCredential {
        match &self.connection_use {
            Some(authority) => credential.with_use_authority(authority.clone()),
            None => credential,
        }
    }

    pub(super) fn request_credential(
        &self,
        account: &AccountId,
        version: &CredentialVersion,
    ) -> Result<Option<OpenerCredential>, CredentialRefusal> {
        self.validate_root()?;
        if let Some(vault) = &self.vault {
            if vault.azure_cli(&self.root)? {
                let authority = self
                    .connection_use
                    .clone()
                    .ok_or(CredentialRefusal::GrantStorageMismatch)?;
                let authenticator =
                    super::azure::Authenticator::new(self.root.clone(), vault.clone(), authority);
                let credential = OpenerCredential::inject_azure(
                    account.clone(),
                    version.clone(),
                    std::sync::Arc::new(authenticator),
                    vault.api_origin(),
                )
                .map_err(CredentialRefusal::Capability)?;
                self.validate_root()?;
                return Ok(Some(credential));
            }
        }
        if let Some(credential) = self.adc_credential(account, version)? {
            return Ok(Some(credential));
        }
        self.aws_credential(account, version)
    }

    pub(super) fn adc_credential(
        &self,
        account: &AccountId,
        version: &CredentialVersion,
    ) -> Result<Option<OpenerCredential>, CredentialRefusal> {
        self.validate_root()?;
        let Some(session) = &self.adc else {
            return Ok(None);
        };
        let vault = self
            .vault
            .as_ref()
            .ok_or(CredentialRefusal::GrantStorageMismatch)?;
        vault.check_adc(&self.root)?;
        let authority = self
            .connection_use
            .clone()
            .ok_or(CredentialRefusal::GrantStorageMismatch)?;
        let authenticator = super::adc::Authenticator::new(
            self.root.clone(),
            vault.clone(),
            session.clone(),
            authority,
        );
        self.validate_root()?;
        OpenerCredential::inject_vertex_adc(
            account.clone(),
            version.clone(),
            std::sync::Arc::new(authenticator),
            vault.api_origin(),
        )
        .map(Some)
        .map_err(CredentialRefusal::Capability)
    }

    pub(super) fn aws_credential(
        &self,
        account: &AccountId,
        version: &CredentialVersion,
    ) -> Result<Option<OpenerCredential>, CredentialRefusal> {
        self.validate_root()?;
        let Some(vault) = &self.vault else {
            return Ok(None);
        };
        let Some(selection) = vault.aws_selection(&self.root)? else {
            return if self.aws.is_some() {
                Err(CredentialRefusal::SourceChanged)
            } else {
                Ok(None)
            };
        };
        if self.aws.as_ref() != Some(&selection) {
            return Err(CredentialRefusal::SourceChanged);
        }
        let authority = self
            .connection_use
            .clone()
            .ok_or(CredentialRefusal::GrantStorageMismatch)?;
        let signer =
            super::aws::Signer::new(self.root.clone(), vault.clone(), selection, authority);
        self.validate_root()?;
        OpenerCredential::inject_aws(
            account.clone(),
            version.clone(),
            std::sync::Arc::new(signer),
            vault.api_origin(),
        )
        .map(Some)
        .map_err(CredentialRefusal::Capability)
    }

    pub(super) fn capture(&self) -> Result<String, CredentialRefusal> {
        self.validate_root()?;
        let material = if let Some(vault) = &self.vault {
            vault.capture(&self.root)
        } else {
            match self.storage {
                CredentialStorageMode::PlaintextFile => {
                    CredentialAuthority::file_material(&self.root, &self.name)
                }
                CredentialStorageMode::ProcessEnvironment => {
                    CredentialAuthority::environment_material(self.environment.as_deref())
                }
                _ => Err(CredentialRefusal::GrantStorageMismatch),
            }
        }?;
        self.validate_root()?;
        Ok(material)
    }
}

/// A consuming initial resolution may retain its exact source for later
/// requests. No constructor takes a caller-selected source or new coordinates.
#[allow(
    missing_debug_implementations,
    reason = "the owner holds credential material and a sealed source"
)]
pub struct ReloadableCredentialAuthority {
    grant: HeldGrant,
    source: BoundSource,
    material: Option<Secret<String>>,
    revision: CredentialRevision,
}

impl CredentialAuthority {
    /// Refuses credentials from store-free preparation. That lane must use
    /// one-shot [`Self::resolve`] instead of installing a profile-renewal owner.
    pub fn into_refreshable(
        self,
        request: CredentialResolutionRequest<'_>,
    ) -> Result<(OpenerCredential, ReloadableCredentialAuthority), CredentialRefusal> {
        self.validate(&request)?;
        let Self {
            credential,
            grant,
            source,
        } = self;
        let source = source.ok_or(CredentialRefusal::RenewalDisabled)?;
        let credential = source.bind_use(credential);
        Ok((
            credential,
            ReloadableCredentialAuthority {
                grant,
                source,
                material: None,
                revision: CredentialRevision::new(0),
            },
        ))
    }
}

impl ReloadableCredentialAuthority {
    pub fn is_vertex_adc(&self) -> bool {
        self.source.adc.is_some()
    }

    /// Check issuance and the original directory before another credential
    /// owner reads or renews the profile's authoritative sign-in state.
    pub fn validate_source(&self) -> Result<(), CredentialRefusal> {
        let target = &self.grant.issuance;
        if !target.is_current_issuance() {
            return Err(CredentialRefusal::StaleCapability {
                issued_at: target.issuance_generation(),
                current: target.current_catalog_generation(),
            });
        }
        self.source.validate_root()
    }
    /// Dispatch and refresh admission are independent of delayed file cleanup.
    pub fn validate_dispatch(&self) -> Result<(), CredentialRefusal> {
        self.validate_source()?;
        if let Some(authority) = &self.source.connection_use {
            authority.check()?;
        }
        Ok(())
    }

    pub fn sign_in_lock_authority(
        &self,
    ) -> Option<std::sync::Arc<dyn grapt_login::chatgpt::SignInLockAuthority>> {
        self.source.connection_use.as_ref().map(|authority| {
            authority.clone() as std::sync::Arc<dyn grapt_login::chatgpt::SignInLockAuthority>
        })
    }

    /// This exact grant's configured sign-in, never another connection's files.
    pub fn chatgpt_sign_in(&self) -> Option<(&Path, &grapt_login::chatgpt::SignInFiles)> {
        if self.source.storage != CredentialStorageMode::PlaintextFile
            && self.source.vault.is_none()
        {
            return None;
        }
        self.source
            .chatgpt_files
            .as_ref()
            .map(|files| (self.source.root.as_path(), files))
    }

    /// Capture staged material before publication; dispatch remains a use-time check.
    pub fn vertex_session(&self) -> Result<grapt_login::vertex::Session, CredentialRefusal> {
        self.validate_source()?;
        let session = self
            .source
            .vault
            .as_ref()
            .ok_or(CredentialRefusal::GrantStorageMismatch)?
            .vertex_session(&self.source.root)?;
        self.validate_source()?;
        Ok(session)
    }

    /// Recheck original key and generation after an exchange, then bind only its access token.
    pub fn current_vertex(
        &mut self,
        session: &grapt_login::vertex::Session,
    ) -> Result<CurrentCredential, CredentialRefusal> {
        self.validate_dispatch()?;
        self.source
            .vault
            .as_ref()
            .ok_or(CredentialRefusal::GrantStorageMismatch)?
            .check_vertex_key(&self.source.root, session)?;
        self.validate_dispatch()?;
        self.with_material(session.token().access().to_owned())
    }

    pub fn watsonx_session(&self) -> Result<grapt_login::watsonx::Session, CredentialRefusal> {
        self.validate_source()?;
        let session = self
            .source
            .vault
            .as_ref()
            .ok_or(CredentialRefusal::GrantStorageMismatch)?
            .watsonx_session(&self.source.root)?;
        self.validate_source()?;
        Ok(session)
    }
    pub fn current_watsonx(
        &mut self,
        session: &grapt_login::watsonx::Session,
    ) -> Result<CurrentCredential, CredentialRefusal> {
        self.validate_dispatch()?;
        self.source
            .vault
            .as_ref()
            .ok_or(CredentialRefusal::GrantStorageMismatch)?
            .check_watsonx_key(&self.source.root, session)?;
        self.validate_dispatch()?;
        self.with_material(session.token().access().to_owned())
    }

    /// Capture under the same file and header guards as initial load.
    pub fn current(&mut self) -> Result<CurrentCredential, CredentialRefusal> {
        self.validate_source()?;
        if let Some(credential) = self
            .source
            .request_credential(&self.grant.account, &self.grant.version)?
        {
            return Ok(CurrentCredential {
                credential: self.source.bind_use(credential),
                revision: self.revision,
            });
        }
        self.with_material(self.source.capture()?)
    }

    fn with_material(
        &mut self,
        mut material: String,
    ) -> Result<CurrentCredential, CredentialRefusal> {
        let retained = material.trim_end_matches(['\r', '\n']).len();
        material.truncate(retained);
        if material.is_empty() {
            return Err(CredentialRefusal::Empty);
        }
        let credential = OpenerCredential::inject(
            self.grant.account.clone(),
            self.grant.version.clone(),
            self.grant.placement,
            Secret::new(material.clone()),
        )
        .map_err(CredentialRefusal::Capability)?;
        let credential = self.source.bind(credential)?;
        let credential = self.source.bind_use(credential);
        if self
            .material
            .as_ref()
            .is_some_and(|previous| previous.expose() != &material)
        {
            self.revision = self
                .revision
                .next()
                .ok_or(CredentialRefusal::RenewalExhausted)?;
        }
        self.material = Some(Secret::new(material));
        Ok(CurrentCredential {
            credential,
            revision: self.revision,
        })
    }
}
