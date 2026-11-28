//! Keys in the operating system's keystore, reached through `keyring`.
use std::io;
use zeroize::Zeroizing;

#[cfg(not(any(test, feature = "loopback-testkit")))]
const SERVICE: &str = "grapt-credential-key";
/// Tests, in this crate and in every crate that tests with its testkit, never
/// read or write a production entry.
#[cfg(any(test, feature = "loopback-testkit"))]
const SERVICE: &str = "grapt-credential-key-test";

/// The secret stored under `name`, or `None` when there is no such entry.
pub(super) fn get(name: &str) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
    match entry(name)?.get_secret() {
        Ok(secret) => Ok(Some(Zeroizing::new(secret))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(refused(error)),
    }
}

/// Store `secret` under `name`.
pub(super) fn set(name: &str, secret: &[u8]) -> io::Result<()> {
    entry(name)?.set_secret(secret).map_err(refused)
}

fn entry(name: &str) -> io::Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, name).map_err(refused)
}

/// A keystore that cannot be reached is unavailable, which is a different
/// fact from one that answered and refused.
fn refused(error: keyring::Error) -> io::Error {
    match error {
        keyring::Error::PlatformFailure(_) | keyring::Error::NoStorageAccess(_) => {
            super::unavailable(format!("the system keystore is unavailable: {error}"))
        }
        other => io::Error::other(format!("the system keystore refused: {other}")),
    }
}
