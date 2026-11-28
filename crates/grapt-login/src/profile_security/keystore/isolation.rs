//! The explicit seam that gives a test process a keystore of its own.
//!
//! Builds with the test kit still use the system keystore unless a test asks
//! otherwise, so a build with every feature enabled protects credentials as
//! a release does. A test process opts in with [`isolate_test_keystore`]; a
//! child it starts shares that keystore only when the test passes the
//! directory in [`TEST_KEYSTORE_VARIABLE`].
use std::io;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Names the test keystore directory for a child process a test starts.
pub const TEST_KEYSTORE_VARIABLE: &str = "GRAPT_TEST_KEYSTORE_DIR";

static ISOLATED: OnceLock<PathBuf> = OnceLock::new();

/// Keep this process's keys in a test keystore and return its directory: the
/// one this process was given in [`TEST_KEYSTORE_VARIABLE`], or else its own.
pub fn isolate_test_keystore() -> io::Result<PathBuf> {
    if let Some(root) = ISOLATED.get() {
        return Ok(root.clone());
    }
    let root = match given()? {
        Some(root) => root,
        None => own(),
    };
    Ok(ISOLATED.get_or_init(|| root).clone())
}

/// Off Windows, a directory of this process's own: the temporary directory
/// may be shared by every account.
#[cfg(not(windows))]
fn own() -> PathBuf {
    std::env::temp_dir().join(format!("grapt-test-keystore-{}", std::process::id()))
}

/// On Windows, one directory in this account's own temporary directory, so
/// test runs share one Credential Manager test key rather than adding one
/// each; the pointer's lock keeps concurrent runs to that key.
#[cfg(windows)]
fn own() -> PathBuf {
    std::env::temp_dir().join("grapt-test-keystore")
}

/// The test keystore this process uses, or `None` for the system keystore.
/// This crate's own tests always use one.
pub(super) fn root() -> io::Result<Option<PathBuf>> {
    if let Some(root) = ISOLATED.get() {
        return Ok(Some(root.clone()));
    }
    #[cfg(test)]
    return isolate_test_keystore().map(Some);
    #[cfg(not(test))]
    given()
}

fn given() -> io::Result<Option<PathBuf>> {
    let Some(value) = std::env::var_os(TEST_KEYSTORE_VARIABLE) else {
        return Ok(None);
    };
    let root = PathBuf::from(value);
    if root.is_absolute() {
        Ok(Some(root))
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{TEST_KEYSTORE_VARIABLE} must name an absolute directory"),
        ))
    }
}
