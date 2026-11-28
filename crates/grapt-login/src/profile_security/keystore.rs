//! Credential sealing keys, held in the operating system's keystore: Windows
//! Credential Manager, the macOS Keychain or the Linux Secret Service.
//!
//! Every key has a random identity, stored in each envelope it seals and in
//! the name of its own keystore entry. An entry is written once, under a name
//! no other key has, and never replaced, so no process and no machine sharing
//! an account can overwrite a key another envelope needs. Which key seals next
//! is a local choice: a pointer file in a private directory of the user's
//! local application data names it, and a lock beside the pointer keeps
//! concurrent first uses of one machine to one new key. A pointer that is
//! missing or names a key the keystore no longer holds only leads to a new
//! key; nothing sealed earlier depends on the pointer.
use super::entropy;
use super::envelope::{ID_BYTES, KEY_BYTES};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// The operating system's keystore.
#[cfg(any(windows, not(any(test, feature = "loopback-testkit"))))]
mod os;
#[cfg(any(windows, not(any(test, feature = "loopback-testkit"))))]
use os as store;
/// Tests off Windows keep keys in files, so no test needs a running Secret
/// Service or touches the user's Keychain.
#[cfg(all(not(windows), any(test, feature = "loopback-testkit")))]
pub(super) mod test_store;
#[cfg(all(not(windows), any(test, feature = "loopback-testkit")))]
use test_store as store;

/// Marks a keystore that cannot be reached, as on a machine with no Secret
/// Service: a different fact from a key that is missing or a keystore that
/// refused.
#[derive(Debug)]
struct Unavailable(String);

impl std::fmt::Display for Unavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Unavailable {}

#[cfg_attr(
    all(not(windows), not(test), feature = "loopback-testkit"),
    expect(
        dead_code,
        reason = "a testkit build's file keystore is always reachable"
    )
)]
fn unavailable(detail: String) -> io::Error {
    io::Error::other(Unavailable(detail))
}

/// Whether an error says the keystore could not be reached at all.
pub(super) fn is_unavailable(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.downcast_ref::<Unavailable>().is_some())
}

const POINTER: &str = "credential-key";
const LOCK: &str = "credential-key.lock";

/// One sealing key and the identity its envelopes carry.
pub(super) struct Key {
    pub(super) id: [u8; ID_BYTES],
    pub(super) secret: Zeroizing<[u8; KEY_BYTES]>,
}

/// The key an envelope names, if the keystore still holds it.
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
/// the keystore no longer holds its key.
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
    if let Some(base) = parent.parent() {
        fs::create_dir_all(base)?;
    }
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
    #[cfg(windows)]
    let reparse = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let reparse = false;
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
    let name = hex(&id);
    if store::get(&name)?.is_some() {
        return Err(io::Error::other(
            "a new credential key identity is already in use",
        ));
    }
    store::set(&name, &*secret)?;
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
    let Some(secret) = store::get(&hex(&id))? else {
        return Ok(None);
    };
    let key: [u8; KEY_BYTES] = secret.as_slice().try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "a credential sealing key in the system keystore is malformed; it was left in place",
        )
    })?;
    Ok(Some(Key {
        id,
        secret: Zeroizing::new(key),
    }))
}

/// A directory of its own in the user's local application data, never a
/// roaming or temporary place.
#[cfg(all(windows, not(any(test, feature = "loopback-testkit"))))]
fn directory() -> io::Result<PathBuf> {
    absolute_variable("LOCALAPPDATA").map(|path| path.join("grapt").join("credential-keys"))
}

/// The user's data directory: `$XDG_DATA_HOME`, or `~/.local/share`.
#[cfg(all(
    not(windows),
    not(target_os = "macos"),
    not(any(test, feature = "loopback-testkit"))
))]
fn directory() -> io::Result<PathBuf> {
    absolute_variable("XDG_DATA_HOME")
        .or_else(|_| absolute_variable("HOME").map(|home| home.join(".local").join("share")))
        .map(|path| path.join("grapt").join("credential-keys"))
}

#[cfg(all(target_os = "macos", not(any(test, feature = "loopback-testkit"))))]
fn directory() -> io::Result<PathBuf> {
    absolute_variable("HOME").map(|home| {
        home.join("Library")
            .join("Application Support")
            .join("grapt")
            .join("credential-keys")
    })
}

#[cfg(not(any(test, feature = "loopback-testkit")))]
fn absolute_variable(name: &str) -> io::Result<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{name} is unknown, so no credential key can be chosen"),
            )
        })
}

/// One test key per account, shared by every test process through the lock,
/// so test runs never accumulate keystore entries.
#[cfg(any(test, feature = "loopback-testkit"))]
fn directory() -> io::Result<PathBuf> {
    Ok(test_root().join("credential-keys"))
}

/// Where tests keep keystore state. Off Windows it is a fixed place rather
/// than the process's temporary directory, because a real keystore does not
/// change with the environment, and a child process started with a cleared
/// environment must find the keys its parent made.
#[cfg(any(test, feature = "loopback-testkit"))]
fn test_root() -> PathBuf {
    #[cfg(unix)]
    let base = PathBuf::from("/tmp");
    #[cfg(not(unix))]
    let base = std::env::temp_dir();
    base.join("grapt-login-test")
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
