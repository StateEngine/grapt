//! A connection incarnation survives key rotation and ends at disconnect.
use super::*;
use grapt_connections::store::Store;
use grapt_provider::{CredentialError, CredentialUseAuthority};
use std::{path::PathBuf, sync::Arc};

pub(super) struct ConnectionUse {
    root: PathBuf,
    identity: same_file::Handle,
    provider: ProviderId,
    namespace: String,
    files: Option<grapt_login::chatgpt::SignInFiles>,
}
fn namespace(file: &str) -> &str {
    let stem = file.strip_suffix(".credential").unwrap_or(file);
    if stem.starts_with("connect-") {
        stem.strip_suffix("-a")
            .or_else(|| stem.strip_suffix("-b"))
            .unwrap_or(stem)
    } else {
        file
    }
}
impl ConnectionUse {
    pub(super) fn from_grant(
        root: &Path,
        grant: &CredentialGrant,
    ) -> Result<Option<Arc<Self>>, CredentialRefusal> {
        if grant.credential_format().binding().is_none()
            && Store::read_snapshot(root)
                .map_err(CredentialRefusal::Read)?
                .is_none()
        {
            return Ok(None);
        }
        Ok(Some(Arc::new(Self {
            root: root.into(),
            identity: same_file::Handle::from_path(root)
                .map_err(CredentialRefusal::RootMetadata)?,
            provider: grant.provider().clone(),
            namespace: namespace(grant.source().as_str()).into(),
            files: grant.chatgpt_files().cloned(),
        })))
    }
    pub(super) fn check(&self) -> Result<(), CredentialRefusal> {
        let current =
            same_file::Handle::from_path(&self.root).map_err(CredentialRefusal::RootMetadata)?;
        if current != self.identity {
            return Err(CredentialRefusal::RootChanged);
        }
        let records = Store::read_snapshot(&self.root).map_err(CredentialRefusal::Read)?;
        if records.is_some_and(|records| {
            records.connections.iter().any(|record| {
                record.id == self.provider && namespace(&record.credential_file) == self.namespace
            })
        }) {
            Ok(())
        } else {
            Err(CredentialRefusal::Absent)
        }
    }
}
impl CredentialUseAuthority for ConnectionUse {
    fn validate(&self) -> Result<(), CredentialError> {
        self.check().map_err(|error| match error {
            CredentialRefusal::Absent => CredentialError::Revoked,
            _ => CredentialError::UseAuthorityUnavailable,
        })
    }
}

impl grapt_login::chatgpt::SignInLockAuthority for ConnectionUse {
    fn try_own(
        &self,
        profile: &Path,
        files: &grapt_login::chatgpt::SignInFiles,
    ) -> std::io::Result<grapt_login::chatgpt::SignInFileLock> {
        use std::io::{Error, ErrorKind};
        if profile != self.root.as_path() || self.files.as_ref() != Some(files) {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "foreign sign-in owner",
            ));
        }
        let admitted = || {
            self.check().map_err(|error| match error {
                CredentialRefusal::Absent => {
                    Error::new(ErrorKind::PermissionDenied, "connection was removed")
                }
                _ => Error::other("connection authority is unavailable"),
            })
        };
        // The historical global lock is retained and bounded. Recover a
        // legacy switch under it before opening the connection writer.
        if files == &grapt_login::chatgpt::SignInFiles::legacy() {
            let owner = files.try_lock(profile)?;
            owner.recover_interrupted_switch()?;
            let _store = Store::open(profile)?;
            admitted()?;
            return Ok(owner);
        }
        // Take the metadata writer before checking or creating a per-slot
        // lock. Deletion uses the same order, so a paused worker cannot
        // recreate a retired namespace after cleanup.
        let _store = Store::open(profile)?;
        admitted()?;
        files.try_lock(profile)
    }
}

impl CredentialGrant {
    pub fn sign_in_lock_authority(
        &self,
        root: &Path,
    ) -> Result<Option<Arc<dyn grapt_login::chatgpt::SignInLockAuthority>>, CredentialRefusal> {
        let root = std::fs::canonicalize(root).map_err(CredentialRefusal::RootResolve)?;
        ConnectionUse::from_grant(&root, self).map(|authority| {
            authority
                .map(|authority| authority as Arc<dyn grapt_login::chatgpt::SignInLockAuthority>)
        })
    }
}

#[cfg(test)]
mod environment_tests {
    use super::*;

    #[test]
    fn retained_environment_grant_ends_at_managed_disconnect_without_reading_the_value(
    ) -> Result<(), Box<dyn std::error::Error>> {
        struct Profile(PathBuf);
        impl Drop for Profile {
            fn drop(&mut self) {
                let _cleanup = std::fs::remove_dir_all(&self.0);
            }
        }
        let root = Profile(
            std::env::temp_dir().join(format!("grapt-env-revocation-{}", std::process::id())),
        );
        std::fs::create_dir(&root.0)?;
        grapt_login::profile_security::protect_directory(&root.0)?;
        let config = br#"{"store":{"authority":"open-ready"},"product":{"build":"env-revocation"},
            "provider":{"id":"openai","model":"test-model","base_model":"test-model","family":"test-model",
                "protocol_family":"openai-responses","account":"api-key","route":"responses",
                "credential_version":"env-v1","policy_version":"env-policy","credential":"env:OPENAI_API_KEY","variants":[]}}"#;
        std::fs::write(root.0.join("grapt.jsonc"), config)?;
        Store::open(&root.0)?.migrate(config)?;
        grapt_login::profile_security::replace_private_file(
            &root.0,
            "account-models.json",
            br#"{"data":[{"id":"test-model","max_model_len":131072}]}"#,
        )?;
        let published = crate::PublishedConfig::load(&root.0)?;
        let authority = ConnectionUse::from_grant(&root.0, published.credentials())?
            .ok_or("managed environment grant omitted disconnect authority")?;
        authority.validate()?;
        Store::open(&root.0)?.save(&grapt_connections::store::Connections {
            connections: Vec::new(),
            default_model: None,
        })?;
        assert!(
            matches!(authority.validate(), Err(CredentialError::Revoked)),
            "a retained environment grant survived managed disconnect"
        );
        Ok(())
    }
}
