//! Copyright 2025 OpenAI. Modified by Grapt: private bounded snapshots,
//! destination binding and the existing protected atomic-file writer.
use super::*;
use crate::profile_security::{self, LockFailure};
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
        Ok(self.read_stored(key)?.map(|(document, _)| document))
    }

    /// The entry and how its file is protected.
    fn read_stored(&self, key: &VaultKey) -> Result<Option<(Document, BackendKind)>, VaultError> {
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
        let plaintext =
            Zeroizing::new(profile_security::open(&stored, BYTE_CAP).map_err(|error| {
                if profile_security::is_key_missing(&error) {
                    VaultError::KeyMissing
                } else if profile_security::is_keystore_unavailable(&error) {
                    VaultError::KeystoreUnavailable
                } else {
                    unavailable(StorageOperation::OpenEnvelope)(error)
                }
            })?);
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
        let backend = if profile_security::is_keystore_sealed(&stored) {
            BackendKind::SystemKeystoreEncryption
        } else if profile_security::is_protected(&stored) {
            BackendKind::WindowsCurrentUserEncryption
        } else {
            BackendKind::OwnerOnlyLegacyPlaintext
        };
        Ok(Some((document, backend)))
    }

    fn lock(&self) -> Result<MutationLock, VaultError> {
        self.check_root()?;
        let path = self.root.join("credential-vault.lock");
        let file = profile_security::open_private_lock(&path).map_err(|failure| match failure {
            LockFailure::Create(error) => unavailable(StorageOperation::CreateLock)(error),
            LockFailure::Protect(error) => unavailable(StorageOperation::ProtectLock)(error),
            LockFailure::Open(error) => unavailable(StorageOperation::OpenLock)(error),
            LockFailure::Verify(error) => unavailable(StorageOperation::VerifyLock)(error),
            LockFailure::Metadata(error) => unavailable(StorageOperation::LockMetadata)(error),
            LockFailure::NotEmpty => VaultError::Corrupt,
        })?;
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

/// Whether an entry exists that may be replaced or removed: a readable one of
/// this connection, or one whose sealing key is gone. Anything else refuses.
/// Whether an entry exists that may be removed: one that may be replaced, or
/// one whose keystore cannot be reached, since removing it overwrites nothing.
fn removable(read: Result<Option<Document>, VaultError>) -> Result<bool, VaultError> {
    match read {
        Err(VaultError::KeystoreUnavailable) => Ok(true),
        read => replaceable(read),
    }
}

fn replaceable(read: Result<Option<Document>, VaultError>) -> Result<bool, VaultError> {
    match read {
        Ok(document) => Ok(document.is_some()),
        Err(VaultError::KeyMissing) => Ok(true),
        Err(error) => Err(error),
    }
}

impl SealedFileVault {
    /// Store `secret` as `key`, the next generation of the connection whose
    /// current material is `predecessor`. While no keystore is present, this
    /// refuses as keystore unavailable when either file is sealed, so a
    /// reconnect never succeeds a sealed credential with an unsealed one.
    pub fn put_succeeding(
        &mut self,
        key: &VaultKey,
        predecessor: &VaultKey,
        binding: &DestinationBinding,
        secret: VaultSecret,
        updated: Timestamp,
    ) -> Result<(), VaultError> {
        self.store(key, Some(predecessor), binding, secret, updated)
    }

    fn store(
        &mut self,
        key: &VaultKey,
        predecessor: Option<&VaultKey>,
        binding: &DestinationBinding,
        secret: VaultSecret,
        updated: Timestamp,
    ) -> Result<(), VaultError> {
        if !binding.valid()
            || secret.0.expose().oauth() != binding.has_refresh()
            || predecessor.is_some_and(|predecessor| predecessor.connection != key.connection)
        {
            return Err(VaultError::InvalidInput);
        }
        let _lock = self.lock()?;
        // Never replace an entry belonging to another connection or silently
        // erase corrupt/legacy storage while trying to reconnect. An entry
        // whose sealing key is gone can only be replaced, so replacing it is
        // the way back.
        replaceable(self.read(key))?;
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
        let target = self.root.join(&key.file);
        let succeeded = predecessor.map(|predecessor| self.root.join(&predecessor.file));
        let targets: Vec<&Path> = [Some(target.as_path()), succeeded.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        let stored = Zeroizing::new(
            profile_security::seal_replacing(&plaintext, &targets).map_err(|error| {
                if profile_security::is_keystore_unavailable(&error) {
                    VaultError::KeystoreUnavailable
                } else {
                    unavailable(StorageOperation::Seal)(error)
                }
            })?,
        );
        self.check_root()?;
        profile_security::replace_private_file(&self.root, &key.file, &stored)
            .map_err(unavailable(StorageOperation::WriteEntry))?;
        self.sync()
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
        self.store(key, None, binding, secret, updated)
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
        // An entry whose sealing key is gone cannot be read; removing the file
        // this key names is how it is cleared.
        if !removable(self.read(key))? {
            self.sync()?;
            return Ok(false);
        }
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
        let Some((document, backend)) = self.read_stored(key)? else {
            return Ok(None);
        };
        if &document.binding != binding {
            return Err(VaultError::BindingMismatch);
        }
        Ok(Some(CredentialDescription {
            backend,
            last4: document.material.last4(),
            updated: Timestamp::from_millis(document.updated_ms),
        }))
    }
}
