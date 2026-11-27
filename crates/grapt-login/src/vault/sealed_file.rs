//! Copyright 2025 OpenAI. Modified by Grapt: private bounded snapshots,
//! destination binding and the existing protected atomic-file writer.
use super::*;
use crate::profile_security;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

const SCHEMA: &str = "secode.credential-vault.v1";
const BYTE_CAP: usize = 32 * 1024;
const PREFIX: &[u8] = b"secode.credential-vault.v1\n";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: String,
    connection: String,
    binding: DestinationBinding,
    updated_ms: i64,
    material: material::Material,
}

// Explicit release also frees a lock whose descriptor was briefly inherited
// by an unrelated concurrent process spawn before its close-on-exec boundary.
struct MutationLock(File);
impl Drop for MutationLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub struct SealedFileVault {
    root: PathBuf,
    identity: same_file::Handle,
}
impl std::fmt::Debug for SealedFileVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SealedFileVault(<private-profile>)")
    }
}

fn unavailable(operation: StorageOperation) -> impl FnOnce(io::Error) -> VaultError {
    move |error| {
        if profile_security::failure_code(&error) == "io" {
            VaultError::Unavailable(StorageFailure::capture(operation, &error))
        } else {
            VaultError::Protection(format!(
                "{operation:?}: {}",
                profile_security::failure_diagnostic(&error)
            ))
        }
    }
}

impl SealedFileVault {
    pub fn open(root: &Path) -> Result<Self, VaultError> {
        profile_security::verify_directory(root)
            .map_err(unavailable(StorageOperation::VerifyDirectory))?;
        let root =
            std::fs::canonicalize(root).map_err(unavailable(StorageOperation::CanonicalizeRoot))?;
        let identity = same_file::Handle::from_path(&root)
            .map_err(unavailable(StorageOperation::RootIdentity))?;
        Ok(Self { root, identity })
    }

    fn check_root(&self) -> Result<(), VaultError> {
        profile_security::verify_directory(&self.root)
            .map_err(unavailable(StorageOperation::VerifyDirectory))?;
        if same_file::Handle::from_path(&self.root)
            .map_err(unavailable(StorageOperation::RootIdentity))?
            != self.identity
        {
            return Err(VaultError::BindingMismatch);
        }
        Ok(())
    }

    fn read(&self, key: &VaultKey) -> Result<Option<Document>, VaultError> {
        self.check_root()?;
        let stored = match profile_security::read_owner_only_file(
            &self.root.join(&key.file),
            profile_security::stored_bound(BYTE_CAP),
        ) {
            Ok(bytes) => Zeroizing::new(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(unavailable(StorageOperation::ReadEntry)(error)),
        };
        #[cfg(windows)]
        if !profile_security::is_protected(&stored) {
            return Err(VaultError::LegacyImportRequired);
        }
        let plaintext = Zeroizing::new(
            profile_security::open(&stored, BYTE_CAP)
                .map_err(unavailable(StorageOperation::OpenEnvelope))?,
        );
        self.check_root()?;
        let bytes = plaintext
            .strip_prefix(PREFIX)
            .ok_or(VaultError::LegacyImportRequired)?;
        let document: Document = serde_json::from_slice(bytes).map_err(|_| VaultError::Corrupt)?;
        if document.schema != SCHEMA
            || !document.binding.valid()
            || !document.material.valid()
            || document.binding.has_refresh() != document.material.oauth()
        {
            return Err(VaultError::Corrupt);
        }
        if document.connection != key.connection.as_str() {
            return Err(VaultError::BindingMismatch);
        }
        Ok(Some(document))
    }

    fn lock(&self) -> Result<MutationLock, VaultError> {
        self.check_root()?;
        let path = self.root.join("credential-vault.lock");
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        #[cfg(windows)]
        std::os::windows::fs::OpenOptionsExt::share_mode(&mut options, 3);
        let file = match options.open(&path) {
            Ok(file) => {
                profile_security::protect_new_private_file(&path, &file)
                    .map_err(unavailable(StorageOperation::ProtectLock))?;
                file
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let mut options = std::fs::OpenOptions::new();
                options.read(true).write(true);
                #[cfg(unix)]
                std::os::unix::fs::OpenOptionsExt::custom_flags(
                    &mut options,
                    libc::O_NOFOLLOW | libc::O_NONBLOCK,
                );
                #[cfg(windows)]
                {
                    use std::os::windows::fs::OpenOptionsExt;
                    options.custom_flags(0x0020_0000).share_mode(3);
                }
                let file = options
                    .open(&path)
                    .map_err(unavailable(StorageOperation::OpenLock))?;
                profile_security::verify_owner_only_file(&path, &file)
                    .map_err(unavailable(StorageOperation::VerifyLock))?;
                if file
                    .metadata()
                    .map_err(unavailable(StorageOperation::LockMetadata))?
                    .len()
                    != 0
                {
                    return Err(VaultError::Corrupt);
                }
                file
            }
            Err(error) => return Err(unavailable(StorageOperation::CreateLock)(error)),
        };
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => VaultError::Busy,
            std::fs::TryLockError::Error(error) => {
                unavailable(StorageOperation::AcquireLock)(error)
            }
        })?;
        self.check_root()?;
        Ok(MutationLock(file))
    }

    fn discard_staged(&self, key: &VaultKey) -> Result<(), VaultError> {
        let path = self.root.join(format!(".{}.new", key.file));
        match profile_security::read_owner_only_file(
            &path,
            profile_security::stored_bound(BYTE_CAP),
        ) {
            Ok(bytes) => {
                let _bytes = Zeroizing::new(bytes);
                std::fs::remove_file(path).map_err(unavailable(StorageOperation::RemoveStaging))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(unavailable(StorageOperation::ReadStaging)(error)),
        }
    }

    fn sync(&self) -> Result<(), VaultError> {
        #[cfg(unix)]
        File::open(&self.root)
            .and_then(|file| file.sync_all())
            .map_err(unavailable(StorageOperation::SyncDirectory))?;
        Ok(())
    }
}

impl CredentialVault for SealedFileVault {
    fn put(
        &mut self,
        key: &VaultKey,
        binding: &DestinationBinding,
        secret: VaultSecret,
        updated: Timestamp,
    ) -> Result<(), VaultError> {
        if !binding.valid() || secret.0.expose().oauth() != binding.has_refresh() {
            return Err(VaultError::InvalidInput);
        }
        let _lock = self.lock()?;
        // Never replace an entry belonging to another connection or silently
        // erase corrupt/legacy storage while trying to reconnect.
        let _previous = self.read(key)?;
        self.discard_staged(key)?;
        let document = Document {
            schema: SCHEMA.into(),
            connection: key.connection.as_str().into(),
            binding: binding.clone(),
            updated_ms: updated.as_millis(),
            material: secret.0.into_inner(),
        };
        let encoded =
            Zeroizing::new(serde_json::to_vec(&document).map_err(|_| VaultError::InvalidInput)?);
        let mut plaintext = Zeroizing::new(Vec::with_capacity(PREFIX.len() + encoded.len()));
        plaintext.extend_from_slice(PREFIX);
        plaintext.extend_from_slice(&encoded);
        if plaintext.len() > BYTE_CAP {
            return Err(VaultError::InvalidInput);
        }
        let stored = Zeroizing::new(
            profile_security::seal(&plaintext).map_err(unavailable(StorageOperation::Seal))?,
        );
        self.check_root()?;
        profile_security::replace_private_file(&self.root, &key.file, &stored)
            .map_err(unavailable(StorageOperation::WriteEntry))?;
        self.sync()
    }

    fn get(
        &self,
        key: &VaultKey,
        binding: &DestinationBinding,
    ) -> Result<Option<VaultSecret>, VaultError> {
        let Some(document) = self.read(key)? else {
            return Ok(None);
        };
        if &document.binding != binding {
            return Err(VaultError::BindingMismatch);
        }
        Ok(Some(VaultSecret(Secret::new(document.material))))
    }

    fn remove(&mut self, key: &VaultKey) -> Result<bool, VaultError> {
        let _lock = self.lock()?;
        self.discard_staged(key)?;
        let Some(_document) = self.read(key)? else {
            self.sync()?;
            return Ok(false);
        };
        self.check_root()?;
        std::fs::remove_file(self.root.join(&key.file))
            .map_err(unavailable(StorageOperation::RemoveEntry))?;
        self.sync()?;
        Ok(true)
    }

    fn describe(
        &self,
        key: &VaultKey,
        binding: &DestinationBinding,
    ) -> Result<Option<CredentialDescription>, VaultError> {
        let Some(document) = self.read(key)? else {
            return Ok(None);
        };
        if &document.binding != binding {
            return Err(VaultError::BindingMismatch);
        }
        Ok(Some(CredentialDescription {
            backend: if cfg!(windows) {
                BackendKind::WindowsCurrentUserEncryption
            } else {
                BackendKind::OwnerOnlyLegacyPlaintext
            },
            last4: document.material.last4(),
            updated: Timestamp::from_millis(document.updated_ms),
        }))
    }
}
