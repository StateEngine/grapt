//! Credential sealing keys, held in Windows Credential Manager.
//!
//! Every key has a random identity, stored in each envelope it seals and in
//! the name of its own Credential Manager entry. An entry is written once,
//! under a name no other key has, and never replaced, so no process and no
//! machine sharing a roaming account can overwrite a key another envelope
//! needs. Which key seals next is a local choice: a pointer file in a private
//! directory of the user's local application data names it, and a lock beside
//! the pointer keeps concurrent first uses of one machine to one new key. A
//! pointer that is missing or names a key Credential Manager no longer holds
//! only leads to a new key; nothing sealed earlier depends on the pointer.
use super::entropy;
use super::envelope::{ID_BYTES, KEY_BYTES};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

#[cfg(not(any(test, feature = "loopback-testkit")))]
const SERVICE: &str = "grapt-credential-key";
/// Tests, in this crate and in every crate that tests with its testkit, never
/// read or write a production entry or pointer.
#[cfg(any(test, feature = "loopback-testkit"))]
const SERVICE: &str = "grapt-credential-key-test";
const POINTER: &str = "credential-key";
const LOCK: &str = "credential-key.lock";

/// One sealing key and the identity its envelopes carry.
pub(super) struct Key {
    pub(super) id: [u8; ID_BYTES],
    pub(super) secret: Zeroizing<[u8; KEY_BYTES]>,
}

/// The key an envelope names, if Credential Manager still holds it.
pub(super) fn by_id(id: &[u8; ID_BYTES]) -> io::Result<Option<Key>> {
    read(*id)
}

/// The key that seals next on this machine, made now if there is none.
pub(super) fn current() -> io::Result<Key> {
    let directory = directory()?;
    // The directory is this user's alone, and neither it nor the app's
    // directory above it is a link, before anything in it is trusted or
    // written.
    prepare(&directory)?;
    if let Some(key) = pointed(&directory)? {
        return Ok(key);
    }
    let lock = super::open_private_lock(&directory.join(LOCK)).map_err(io::Error::from)?;
    lock.lock()?;
    let key = match pointed(&directory) {
        Ok(Some(key)) => Ok(key),
        Ok(None) => create(&directory),
        Err(error) => Err(error),
    };
    // Closing the handle releases the lock even if this unlock fails.
    let _unlocked = lock.unlock();
    key
}

/// The key the pointer names, or `None` when there is no usable pointer or
/// Credential Manager no longer holds its key.
fn pointed(directory: &Path) -> io::Result<Option<Key>> {
    // A link or an oversized file in place of the pointer refuses.
    let bytes = match super::read_owner_only_file(&directory.join(POINTER), ID_BYTES * 2) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    match std::str::from_utf8(&bytes).ok().and_then(parse) {
        Some(id) => read(id),
        None => Ok(None),
    }
}

/// Make the key directory and its app-owned parent, refusing either when it
/// is a link or junction, and make the key directory private. The parent is
/// checked again afterwards, so a parent swapped for a link meanwhile refuses.
fn prepare(directory: &Path) -> io::Result<()> {
    let parent = directory
        .parent()
        .ok_or_else(|| io::Error::other("the credential key directory has no parent"))?;
    create_real_directory(parent)?;
    create_real_directory(directory)?;
    super::protect_directory(directory)?;
    real_directory(parent)
}

fn create_real_directory(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    real_directory(path)
}

/// A directory itself, never a link, junction or other reparse point.
fn real_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let reparse = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() || reparse {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a credential key directory is a link or not a directory; it was left in place",
        ));
    }
    Ok(())
}

/// Holding the lock: write a key under a new name, read it back, then point
/// at it. A crash before the pointer moves leaves an unused entry only.
fn create(directory: &Path) -> io::Result<Key> {
    let mut id = [0; ID_BYTES];
    entropy::fill(&mut id)?;
    let mut secret = Zeroizing::new([0; KEY_BYTES]);
    entropy::fill(&mut *secret)?;
    let entry = entry(id)?;
    match entry.get_secret() {
        Err(keyring::Error::NoEntry) => {}
        Ok(_) => {
            return Err(io::Error::other(
                "a new credential key identity is already in use",
            ))
        }
        Err(error) => return Err(unavailable(error)),
    }
    entry.set_secret(&*secret).map_err(unavailable)?;
    let stored = read(id)?
        .ok_or_else(|| io::Error::other("the new credential sealing key could not be read back"))?;
    if *stored.secret != *secret {
        return Err(io::Error::other(
            "the new credential sealing key read back differently",
        ));
    }
    // Staged through a file created new for this process, then published.
    super::replace_sign_in_file(directory, POINTER, hex(&id).as_bytes())?;
    Ok(stored)
}

fn read(id: [u8; ID_BYTES]) -> io::Result<Option<Key>> {
    match entry(id)?.get_secret() {
        Ok(secret) => {
            let secret = Zeroizing::new(secret);
            let key: [u8; KEY_BYTES] = secret.as_slice().try_into().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "a credential sealing key in Windows Credential Manager is malformed; \
                     it was left in place",
                )
            })?;
            Ok(Some(Key {
                id,
                secret: Zeroizing::new(key),
            }))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(unavailable(error)),
    }
}

fn entry(id: [u8; ID_BYTES]) -> io::Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, &hex(&id)).map_err(unavailable)
}

/// A directory of its own in the user's local application data, never a
/// roaming or temporary place.
#[cfg(not(any(test, feature = "loopback-testkit")))]
fn directory() -> io::Result<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|path| path.join("grapt").join("credential-keys"))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "LOCALAPPDATA is unknown, so no credential key can be chosen",
            )
        })
}

/// One test key per account, shared by every test process through the lock,
/// so test runs never accumulate Credential Manager entries.
#[cfg(any(test, feature = "loopback-testkit"))]
fn directory() -> io::Result<PathBuf> {
    Ok(std::env::temp_dir()
        .join("grapt-login-test")
        .join("credential-keys"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse(text: &str) -> Option<[u8; ID_BYTES]> {
    if text.len() != ID_BYTES * 2 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut id = [0; ID_BYTES];
    for (index, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(id)
}

fn unavailable(error: keyring::Error) -> io::Error {
    io::Error::other(format!(
        "Windows Credential Manager is unavailable: {error}"
    ))
}
