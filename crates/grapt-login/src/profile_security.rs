//! Protection for the existing profile files; never a second credential store.
//!
//! A credential is sealed with AES-256-GCM under a key the operating system's
//! keystore holds (Windows Credential Manager, the macOS Keychain or the Linux
//! Secret Service); the file stays the only store of the credential itself.
//! Credentials stored earlier, sealed with DPAPI or as owner-only plaintext,
//! stay readable and are sealed the new way when next written. Off Windows, a
//! machine with no reachable keystore stores a new credential owner-only, with
//! a warning, and never reads a sealed credential any other way.
//!
//! The DPAPI operation.
//! Copyright 2025 OpenAI. Modified by Grapt; see licenses/openai-codex/.
//! Unlike that sandbox's machine scope, credentials use CurrentUser. The
//! managed Windows adapter avoids unsafe FFI and sends secrets only over pipes.
use std::io;
use std::path::Path;

mod private_read;
pub use private_read::{read_owner_only_file, verify_owner_only_file};
mod batch;
pub use batch::ProtectionBatch;
mod private_lock;
pub(crate) use private_lock::{open_private_lock, LockFailure};
mod replace;
#[cfg(any(windows, test))]
mod request_budget;
#[cfg(any(windows, test))]
mod windows_protocol;
pub use replace::replace_private_file;
pub(crate) use replace::replace_sign_in_file;

mod refusal;
#[cfg(any(windows, test))]
pub(crate) use refusal::child_refusal;
pub use refusal::{failure_code, failure_diagnostic};

mod entropy;
mod envelope;
mod fallback;
pub use fallback::{keystore_fallback, KeystoreFallback, KEYSTORE_FALLBACK};
#[cfg(test)]
pub(crate) mod fixtures;
mod keystore;
#[cfg(all(not(windows), any(test, feature = "loopback-testkit")))]
pub use keystore::test_store::Unavailable as TestKeystoreUnavailable;
#[cfg(any(test, feature = "loopback-testkit"))]
pub use keystore::{isolate_test_keystore, TEST_KEYSTORE_VARIABLE};

/// The earlier Windows format, still opened so existing credentials migrate.
const DPAPI_ENVELOPE: &[u8] = b"secode.dpapi.current-user.v1\n";
const KEYSTORE_ENVELOPE: &[u8] = b"grapt.credential.aes-256-gcm.v1\n";

/// Why a sealed credential cannot be opened when its key is gone, and the step
/// that replaces it. Fixed text: it names no path, key or account.
pub const KEY_MISSING: &str =
    "this credential's sealing key is missing from the system keystore; sign in again or add \
     the credential again";

/// Marks an [`open`] refusal whose envelope names a key the keystore no longer
/// holds. Only replacing the credential gets past it.
#[derive(Debug)]
struct KeyMissing;

impl std::fmt::Display for KeyMissing {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(KEY_MISSING)
    }
}

impl std::error::Error for KeyMissing {}

/// Whether an [`open`] refusal means the credential's sealing key is gone, so
/// the credential can only be replaced, never read.
#[must_use]
pub fn is_key_missing(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.downcast_ref::<KeyMissing>().is_some())
}

/// Why a credential is not sealed when `DBUS_SESSION_BUS_ADDRESS` is set but
/// malformed, and the step that fixes it. Fixed text: it names no value.
pub const BUS_ADDRESS_INVALID: &str =
    "DBUS_SESSION_BUS_ADDRESS is set but is not a valid D-Bus address, so the system keystore \
     was not used; correct or unset it, then try again";

#[cfg(test)]
pub(crate) use keystore::bus_address_invalid;

/// Whether a refusal means `DBUS_SESSION_BUS_ADDRESS` is set but malformed.
#[must_use]
pub fn is_bus_address_invalid(error: &io::Error) -> bool {
    keystore::is_bus_address_invalid(error)
}

/// Whether an [`open`] or [`seal`] refusal means the system keystore could not
/// be reached at all. A sealed credential then waits for the keystore; it is
/// never read or replaced another way.
#[must_use]
pub fn is_keystore_unavailable(error: &io::Error) -> bool {
    keystore::is_unavailable(error)
}

/// Whether bytes are sealed under a keystore key, without opening them.
#[must_use]
pub fn is_keystore_sealed(bytes: &[u8]) -> bool {
    bytes.starts_with(KEYSTORE_ENVELOPE)
}

/// Whether bytes carry a protected format, without attempting decryption.
#[must_use]
pub fn is_protected(bytes: &[u8]) -> bool {
    bytes.starts_with(KEYSTORE_ENVELOPE) || bytes.starts_with(DPAPI_ENVELOPE)
}

/// Finite room for an envelope's header, nonce and tag, and for DPAPI's
/// header, padding and authentication data.
#[must_use]
pub const fn stored_bound(plaintext_bound: usize) -> usize {
    plaintext_bound.saturating_add(1024)
}

/// Protect bytes with the system keystore. Nothing is stored another way:
/// when the keystore cannot seal them, this refuses.
pub fn seal(bytes: &[u8]) -> io::Result<Vec<u8>> {
    seal_with(&keystore::current()?, bytes)
}

/// Protect bytes that will be written as `target`; see [`seal_replacing`].
pub fn seal_for(bytes: &[u8], target: &Path) -> io::Result<Vec<u8>> {
    seal_replacing(bytes, &[target])
}

/// Protect bytes that will take the place of every file in `targets`: the
/// file they are written as, and any file whose credential they succeed.
///
/// Off Windows, when no keystore is present at all, the bytes are returned as
/// they are for the owner-only file and [`keystore_fallback`] reports it, but
/// only while no target holds a sealed credential: a sealed credential is
/// never succeeded by an unsealed one, so that case refuses as keystore
/// unavailable.
pub fn seal_replacing(bytes: &[u8], targets: &[&Path]) -> io::Result<Vec<u8>> {
    let mut sealed = seal_all(&[bytes], targets)?;
    sealed
        .pop()
        .ok_or_else(|| io::Error::other("sealing returned no bytes"))
}

/// Protect several files that are published together as one decision, each
/// item becoming one file and `targets` naming every file they replace or
/// succeed. Either every item is sealed or, under [`seal_replacing`]'s
/// headless rule, every item stays as it is; never a mix.
pub fn seal_all(items: &[&[u8]], targets: &[&Path]) -> io::Result<Vec<Vec<u8>>> {
    match keystore::current() {
        Ok(key) => items.iter().map(|bytes| seal_with(&key, bytes)).collect(),
        #[cfg(not(windows))]
        Err(error) if keystore::is_unavailable(&error) => {
            fallback::permit(targets, error)?;
            Ok(items.iter().map(|bytes| bytes.to_vec()).collect())
        }
        Err(error) => {
            let _ = targets;
            Err(error)
        }
    }
}

fn seal_with(key: &keystore::Key, bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut nonce = [0; envelope::NONCE_BYTES];
    entropy::fill(&mut nonce)?;
    envelope::seal(&key.id, &key.secret, nonce, bytes)
}

/// Decode an envelope in memory, bounding the plaintext independently of storage.
/// Legacy plaintext remains readable; an invalid envelope never falls back to it.
pub fn open(bytes: &[u8], plaintext_bound: usize) -> io::Result<Vec<u8>> {
    let plaintext = if let Some(sealed) = bytes.strip_prefix(KEYSTORE_ENVELOPE) {
        let key = keystore::by_id(&envelope::key_id(sealed)?)?
            .ok_or_else(|| io::Error::other(KeyMissing))?;
        envelope::open(&key.secret, sealed)?
    } else if let Some(protected) = bytes.strip_prefix(DPAPI_ENVELOPE) {
        #[cfg(windows)]
        {
            windows::run("open", protected)?
        }
        #[cfg(not(windows))]
        {
            let _ = protected;
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Windows user-protected credential requires its owning Windows account",
            ));
        }
    } else {
        bytes.to_vec()
    };
    if plaintext.len() > plaintext_bound {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "credential plaintext exceeds its byte bound",
        ));
    }
    Ok(plaintext)
}

/// Restrict an existing real directory to the current user, including inheritance
/// for newly created files. Call before any credential/configuration write.
pub fn protect_directory(path: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "profile protection requires a real directory",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "profile protection refuses a reparse point",
            ));
        }
        let path = path.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "profile protection requires a Unicode path",
            )
        })?;
        windows::run("directory", path.as_bytes())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Check persisted trust storage before consuming a prior decision; never
/// legitimize an insecure registry by changing its permissions at read time.
pub fn verify_directory(path: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "trust storage must be a real private directory",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "trust storage is a reparse point",
            ));
        }
        let path = path.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "trust storage requires a Unicode path",
            )
        })?;
        windows::run("directory-check", path.as_bytes())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "trust storage is accessible to other users",
            ));
        }
    }
    Ok(())
}

/// Establish user ownership on a newly created private trust record. Windows
/// elevated tokens default new objects to their Administrators owner; permit
/// only that token owner to be normalized, then apply the strict read check.
/// Call only after create_new in the already protected trust directory.
pub fn protect_new_private_file(path: &Path, file: &std::fs::File) -> io::Result<()> {
    let opened = same_file::Handle::from_file(file.try_clone()?)?;
    if opened != same_file::Handle::from_path(path)? || !file.metadata()?.is_file() {
        return Err(io::Error::other("new trust record changed identity"));
    }
    #[cfg(windows)]
    {
        let path = path.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "trust record requires a Unicode path",
            )
        })?;
        windows::run("file-owner", path.as_bytes())?;
    }
    verify_private_file(path, file)
}

/// Check a trust record's own authority, not just its parent directory.
/// The caller keeps an opened, no-write/no-delete Windows lease through its
/// read. Identity is checked against that opened object before and after ACL
/// inspection; an unsafe existing record is refused, never repaired into trust.
pub fn verify_private_file(path: &Path, file: &std::fs::File) -> io::Result<()> {
    let opened = same_file::Handle::from_file(file.try_clone()?)?;
    if opened != same_file::Handle::from_path(path)? || !file.metadata()?.is_file() {
        return Err(io::Error::other("trust record changed while opening"));
    }
    #[cfg(windows)]
    {
        let path = path.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "trust record requires a Unicode path",
            )
        })?;
        windows::run("file-check", path.as_bytes())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = file.metadata()?;
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("trust record has no parent"))?;
        if metadata.uid() != std::fs::metadata(parent)?.uid()
            || metadata.permissions().mode() & 0o022 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "trust record is foreign-writable or foreign-owned",
            ));
        }
    }
    if opened != same_file::Handle::from_path(path)? {
        return Err(io::Error::other(
            "trust record changed during authority inspection",
        ));
    }
    Ok(())
}

#[cfg(windows)]
#[path = "profile_security/windows.rs"]
mod windows;

#[cfg(test)]
#[path = "profile_security/tests.rs"]
mod tests;
