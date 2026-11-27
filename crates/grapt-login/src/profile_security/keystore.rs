//! The current user's credential sealing key, held in Windows Credential
//! Manager. It is made once, under a lock every maker takes, and read back
//! before use, so concurrent first uses agree on one key. An entry that exists
//! but cannot be read is an error, never a reason to make another key: that
//! would orphan every credential sealed under the first.
use super::{entropy, envelope::KEY_BYTES};
use std::fs::OpenOptions;
use std::io;
use zeroize::Zeroizing;

const SERVICE: &str = "grapt";
const ACCOUNT: &str = "credential-sealing-key.v1";
/// In the user's own temporary directory; it guards key creation only.
const LOCK: &str = "grapt-credential-sealing-key.lock";

pub(super) type Key = Zeroizing<[u8; KEY_BYTES]>;

/// The key, if one was made.
pub(super) fn existing() -> io::Result<Option<Key>> {
    read(&entry()?)
}

/// The key, made now if this user has none yet.
pub(super) fn get_or_create() -> io::Result<Key> {
    let entry = entry()?;
    if let Some(key) = read(&entry)? {
        return Ok(key);
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(std::env::temp_dir().join(LOCK))?;
    lock.lock()?;
    let key = create(&entry);
    // Closing the handle releases the lock even if this unlock fails.
    let _unlocked = lock.unlock();
    key
}

/// Holding the lock: another maker may have finished first.
fn create(entry: &keyring::Entry) -> io::Result<Key> {
    if let Some(key) = read(entry)? {
        return Ok(key);
    }
    let mut fresh: Key = Zeroizing::new([0; KEY_BYTES]);
    entropy::fill(&mut *fresh)?;
    entry.set_secret(&*fresh).map_err(unavailable)?;
    read(entry)?
        .ok_or_else(|| io::Error::other("the new credential sealing key could not be read back"))
}

fn entry() -> io::Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, ACCOUNT).map_err(unavailable)
}

fn read(entry: &keyring::Entry) -> io::Result<Option<Key>> {
    match entry.get_secret() {
        Ok(secret) => {
            let secret = Zeroizing::new(secret);
            let key: [u8; KEY_BYTES] = secret.as_slice().try_into().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the credential sealing key in Windows Credential Manager is malformed; \
                     it was left in place",
                )
            })?;
            Ok(Some(Zeroizing::new(key)))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(unavailable(error)),
    }
}

fn unavailable(error: keyring::Error) -> io::Error {
    io::Error::other(format!(
        "Windows Credential Manager is unavailable: {error}"
    ))
}
