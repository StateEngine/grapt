//! The warning a credential write gives when no keystore can seal it.
//!
//! Off Windows a machine may have no keystore to reach, such as a server
//! reached over SSH with no Secret Service. A new credential is then stored
//! owner-only (0600), as it was before sealing, and this says so: once on
//! standard error as one JSON line, and to any caller that asks.
use std::io;
use std::sync::OnceLock;

/// The warning's stable code.
pub const KEYSTORE_FALLBACK: &str = "credential-keystore-unavailable";
const DETAIL_BYTES: usize = 512;

/// A credential was stored owner-only because no keystore could seal it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct KeystoreFallback {
    /// Always [`KEYSTORE_FALLBACK`].
    pub warning: &'static str,
    /// Why the keystore could not be reached.
    pub detail: String,
    /// How the credential is stored instead.
    pub stored: &'static str,
    /// The step that seals it.
    pub step: &'static str,
}

#[cfg(target_os = "macos")]
const STEP: &str = "unlock the login Keychain, then sign in or add the credential again to seal it";
#[cfg(not(target_os = "macos"))]
const STEP: &str =
    "start a Secret Service such as GNOME Keyring or KeePassXC, then sign in or add \
                    the credential again to seal it";

static NOTED: OnceLock<KeystoreFallback> = OnceLock::new();

/// The fallback this process took, if any credential write took it.
#[must_use]
pub fn keystore_fallback() -> Option<KeystoreFallback> {
    NOTED.get().cloned()
}

/// Record the fallback, and say it on standard error the first time.
#[cfg_attr(windows, expect(dead_code, reason = "Windows always has its keystore"))]
pub(super) fn note(error: &io::Error) {
    let mut detail = error.to_string();
    if detail.len() > DETAIL_BYTES {
        let mut end = DETAIL_BYTES;
        while !detail.is_char_boundary(end) {
            end -= 1;
        }
        detail.truncate(end);
        detail.push('~');
    }
    let fallback = KeystoreFallback {
        warning: KEYSTORE_FALLBACK,
        detail,
        stored: "owner-only file (mode 0600), not sealed",
        step: STEP,
    };
    if NOTED.set(fallback.clone()).is_ok() {
        match serde_json::to_string(&fallback) {
            Ok(line) => eprintln!("{line}"),
            Err(_) => eprintln!("{KEYSTORE_FALLBACK}: {}", fallback.detail),
        }
    }
}
