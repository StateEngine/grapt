//! The warning a credential write gives when no keystore can seal it.
//!
//! Off Windows a machine may have no keystore to reach, such as a server
//! reached over SSH with no Secret Service. A new credential is then stored
//! owner-only (0600), as it was before sealing, and this says so: once on
//! standard error as one JSON line, and to any caller that asks. A file that
//! is already sealed is never replaced that way.
use std::io;
use std::path::Path;
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
const STEP: &str =
    "restore a default Keychain, then sign in or add the credential again to seal it";
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

/// Allow an unsealed write in place of `targets` because the keystore is
/// absent, or return `absent` when any target holds a sealed credential; a
/// target that cannot be read refuses too.
#[cfg_attr(windows, expect(dead_code, reason = "Windows always has its keystore"))]
pub(super) fn permit(targets: &[&Path], absent: io::Error) -> io::Result<()> {
    for target in targets {
        if sealed(target)? {
            return Err(absent);
        }
    }
    note(&absent);
    Ok(())
}

/// Whether `target` holds a sealed credential. Only its first bytes are read;
/// a missing file holds none, and anything but a regular file refuses.
#[cfg_attr(windows, expect(dead_code, reason = "Windows always has its keystore"))]
fn sealed(target: &Path) -> io::Result<bool> {
    use std::io::Read;
    match std::fs::symlink_metadata(target) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
        Ok(metadata) if !metadata.is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a credential file is not a regular file; it was left in place",
            ))
        }
        Ok(_) => {}
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NOFOLLOW | libc::O_NONBLOCK,
    );
    let mut head = Vec::new();
    let limit = super::KEYSTORE_ENVELOPE
        .len()
        .max(super::DPAPI_ENVELOPE.len());
    options
        .open(target)?
        .take(u64::try_from(limit).unwrap_or(u64::MAX))
        .read_to_end(&mut head)?;
    Ok(super::is_protected(&head))
}

/// Record the fallback, and say it on standard error the first time.
#[cfg_attr(windows, expect(dead_code, reason = "Windows always has its keystore"))]
fn note(error: &io::Error) {
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
