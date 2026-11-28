//! Keys in the operating system's keystore, reached through `keyring`.
use std::io;
use zeroize::Zeroizing;

/// The service every production key entry is stored under.
pub(super) const SERVICE: &str = "grapt-credential-key";
/// Windows tests that configured a test keystore keep keys in the real
/// Credential Manager, under a service no production entry uses.
#[cfg(all(windows, any(test, feature = "loopback-testkit")))]
pub(super) const TEST_SERVICE: &str = "grapt-credential-key-test";

/// The secret stored under `name`, or `None` when there is no such entry.
pub(super) fn get(service: &str, name: &str) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
    match entry(service, name)?.get_secret() {
        Ok(secret) => Ok(Some(Zeroizing::new(secret))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(refused(error)),
    }
}

/// Store `secret` under `name`.
pub(super) fn set(service: &str, name: &str, secret: &[u8]) -> io::Result<()> {
    entry(service, name)?.set_secret(secret).map_err(refused)
}

fn entry(service: &str, name: &str) -> io::Result<keyring::Entry> {
    keyring::Entry::new(service, name).map_err(refused)
}

/// A keystore that is not there at all is unavailable. Every other failure,
/// including a locked keystore, a dismissed prompt or a read-only keychain,
/// is a refusal: a credential is then never stored another way.
fn refused(error: keyring::Error) -> io::Error {
    if absent(&error) {
        super::unavailable(format!("no system keystore is present: {error}"))
    } else {
        io::Error::other(format!(
            "the system keystore refused: {error}; unlock it or approve its prompt, then try \
             again"
        ))
    }
}

/// No D-Bus session, or no Secret Service on it.
#[cfg(target_os = "linux")]
fn absent(error: &keyring::Error) -> bool {
    let no_service = |error: &zbus::fdo::Error| {
        matches!(
            error,
            zbus::fdo::Error::ServiceUnknown(_) | zbus::fdo::Error::NameHasNoOwner(_)
        )
    };
    let keyring::Error::PlatformFailure(inner) = error else {
        return false;
    };
    match inner.downcast_ref::<secret_service::Error>() {
        Some(secret_service::Error::Unavailable) => true,
        Some(secret_service::Error::ZbusFdo(error)) => no_service(error),
        Some(secret_service::Error::Zbus(zbus::Error::FDO(error))) => no_service(error),
        Some(secret_service::Error::Zbus(zbus::Error::MethodError(name, _, _))) => matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.NameHasNoOwner"
        ),
        _ => false,
    }
}

/// No keychain at all: `errSecNotAvailable` or `errSecNoSuchKeychain`.
#[cfg(target_os = "macos")]
fn absent(error: &keyring::Error) -> bool {
    let keyring::Error::NoStorageAccess(inner) = error else {
        return false;
    };
    inner
        .downcast_ref::<security_framework::base::Error>()
        .is_some_and(|error| matches!(error.code(), -25291 | -25294))
}

/// Credential Manager is part of Windows; any failure is a refusal.
#[cfg(windows)]
fn absent(_error: &keyring::Error) -> bool {
    false
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn only_an_absent_secret_service_is_unavailable() {
        let unavailable = |error| super::super::is_unavailable(&refused(error));
        let failure = |error| keyring::Error::PlatformFailure(Box::new(error));
        let no_access = |error| keyring::Error::NoStorageAccess(Box::new(error));
        assert!(unavailable(failure(secret_service::Error::Unavailable)));
        assert!(unavailable(failure(secret_service::Error::ZbusFdo(
            zbus::fdo::Error::ServiceUnknown("org.freedesktop.secrets".into())
        ))));
        assert!(!unavailable(no_access(secret_service::Error::Locked)));
        assert!(!unavailable(no_access(secret_service::Error::Prompt)));
        assert!(!unavailable(no_access(secret_service::Error::NoResult)));
        assert!(!unavailable(failure(secret_service::Error::ZbusFdo(
            zbus::fdo::Error::AccessDenied("refused".into())
        ))));
        assert!(!unavailable(keyring::Error::NoStorageAccess(
            "an unrecognised refusal".into()
        )));
    }
}
