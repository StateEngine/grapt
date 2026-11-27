//! Which build is running, as the release package attests it.
//!
//! The identity is the attested `source_commit` (docs/release-packaging/
//! PACKAGE-IDENTITY-CONTRACT.md §2), never the crate version every crate
//! shares and never whatever checkout compiled the binary. A packaged install
//! carries `release-manifest.json` beside its binaries; this reports its
//! `engine_version` only when the manifest attests these exact executable
//! bytes. Anything else is an unattested build and says so.

use std::fmt;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

/// The manifest a release package installs beside its binaries.
pub const MANIFEST: &str = "release-manifest.json";

/// The largest manifest read.
const MANIFEST_CAP: u64 = 1024 * 1024;

/// The largest executable hashed, the product's own fingerprint bound.
const EXECUTABLE_CAP: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildIdentity {
    /// The manifest beside the binary attests these bytes as this commit.
    Attested { source_commit: String },
    /// No attestation applies; the reason says why.
    Unattested(&'static str),
}

impl fmt::Display for BuildIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Attested { source_commit } => write!(f, "build {source_commit}"),
            Self::Unattested(reason) => write!(f, "build unattested ({reason})"),
        }
    }
}

/// The identity of the running executable.
pub fn of_running_binary() -> BuildIdentity {
    match std::env::current_exe() {
        Ok(binary) => of_binary(&binary),
        Err(_) => BuildIdentity::Unattested("the running executable cannot be located"),
    }
}

/// The identity of `binary` as the manifest beside it attests it.
pub fn of_binary(binary: &Path) -> BuildIdentity {
    use BuildIdentity::Unattested;
    let (Some(directory), Some(name)) = (binary.parent(), binary.file_name()) else {
        return Unattested("the running executable has no directory");
    };
    let manifest = match read_capped(&directory.join(MANIFEST), MANIFEST_CAP) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Unattested("no release-manifest.json beside this binary"),
        Err(()) => return Unattested("release-manifest.json beside this binary is unreadable"),
    };
    let Ok(manifest) = serde_json::from_slice::<serde_json::Value>(&manifest) else {
        return Unattested("release-manifest.json beside this binary is not valid JSON");
    };
    let entry = name
        .to_str()
        .and_then(|name| manifest.get("binaries")?.get(name));
    let attested = text(entry.and_then(|entry| entry.get("sha256")));
    // The binary's own attested source_commit is the identity; the package's
    // engine_version must name the same commit, as the packager requires.
    let commit = text(entry.and_then(|entry| entry.get("source_commit"))).filter(|commit| {
        commit.len() == 40
            && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
            && text(manifest.get("engine_version")) == Some(*commit)
    });
    let (Some(attested), Some(commit)) = (attested, commit) else {
        return Unattested("release-manifest.json does not attest this binary");
    };
    match sha256_hex(binary) {
        Some(actual) if actual.eq_ignore_ascii_case(attested) => BuildIdentity::Attested {
            source_commit: commit.to_owned(),
        },
        Some(_) => Unattested("this binary differs from the one release-manifest.json attests"),
        None => Unattested("the running executable cannot be read"),
    }
}

/// Open a regular file; `Ok(None)` when it is absent. Anything else is
/// refused before it is opened, because opening a FIFO blocks until it has a
/// writer; the open is non-blocking too, and the opened handle must still be
/// that regular file.
fn open_regular(path: &Path) -> Result<Option<std::fs::File>, ()> {
    let before = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    if !before.is_file() {
        return Err(());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NONBLOCK);
    let file = options.open(path).map_err(|_| ())?;
    let opened = file.metadata().map_err(|_| ())?;
    if !opened.is_file() || !same_file(&before, &opened) {
        return Err(());
    }
    Ok(Some(file))
}

#[cfg(unix)]
fn same_file(before: &std::fs::Metadata, opened: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.dev() == opened.dev() && before.ino() == opened.ino()
}

#[cfg(not(unix))]
fn same_file(_before: &std::fs::Metadata, _opened: &std::fs::Metadata) -> bool {
    true
}

fn text(value: Option<&serde_json::Value>) -> Option<&str> {
    value.and_then(serde_json::Value::as_str)
}

/// A regular file of at most `cap` bytes; `Ok(None)` when it is absent.
fn read_capped(path: &Path, cap: u64) -> Result<Option<Vec<u8>>, ()> {
    let Some(file) = open_regular(path)? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    file.take(cap + 1).read_to_end(&mut bytes).map_err(|_| ())?;
    if bytes.len() as u64 > cap {
        return Err(());
    }
    Ok(Some(bytes))
}

fn sha256_hex(path: &Path) -> Option<String> {
    let file = open_regular(path).ok()??;
    let mut reader = file.take(EXECUTABLE_CAP + 1);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = reader.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        total += read as u64;
        hasher.update(&buffer[..read]);
    }
    if total > EXECUTABLE_CAP {
        return None;
    }
    Some(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    /// A directory with a fake binary, removed on drop.
    struct Install(std::path::PathBuf);

    impl Install {
        fn new(case: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "secode-build-identity-{case}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir(&path).unwrap_or_else(|error| panic!("{error}"));
            std::fs::write(path.join("grapt"), b"pretend executable bytes")
                .unwrap_or_else(|error| panic!("{error}"));
            Self(path)
        }

        fn binary(&self) -> std::path::PathBuf {
            self.0.join("grapt")
        }

        fn manifest(&self, sha256: &str, commit: &str) {
            self.manifest_with(sha256, commit, commit);
        }

        /// The shape scripts/release/package.py writes for each binary.
        fn manifest_with(&self, sha256: &str, engine_version: &str, source_commit: &str) {
            let manifest = serde_json::json!({
                "schema": "grapt.release-manifest.v1",
                "engine_version": engine_version,
                "engine_version_is": "the attested source_commit of the built binaries",
                "crate_version_secondary": "0.1.0",
                "binaries": {"grapt": {
                    "sha256": sha256,
                    "bytes": 24,
                    "package": "grapt-cli",
                    "bin": "grapt",
                    "source_commit": source_commit,
                    "profile": "release",
                    "target": "x86_64-unknown-linux-gnu",
                    "build_receipt": "build-receipt.json",
                    "build_receipt_sha256": "0".repeat(64),
                }},
            });
            std::fs::write(self.0.join(MANIFEST), manifest.to_string())
                .unwrap_or_else(|error| panic!("{error}"));
        }
    }

    impl Drop for Install {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    #[test]
    fn a_manifest_that_attests_these_bytes_names_its_commit() {
        let install = Install::new("attested");
        install.manifest(&digest(b"pretend executable bytes"), COMMIT);
        let identity = of_binary(&install.binary());
        assert_eq!(
            identity,
            BuildIdentity::Attested {
                source_commit: COMMIT.to_owned()
            }
        );
        assert_eq!(identity.to_string(), format!("build {COMMIT}"));
    }

    #[test]
    fn a_binary_without_a_manifest_is_unattested() {
        let install = Install::new("bare");
        let identity = of_binary(&install.binary());
        assert_eq!(
            identity.to_string(),
            "build unattested (no release-manifest.json beside this binary)"
        );
    }

    #[test]
    fn a_manifest_for_other_bytes_does_not_lend_its_commit() {
        let install = Install::new("other-bytes");
        install.manifest(&digest(b"a different build"), COMMIT);
        assert_eq!(
            of_binary(&install.binary()),
            BuildIdentity::Unattested(
                "this binary differs from the one release-manifest.json attests"
            )
        );
    }

    #[test]
    fn a_manifest_without_a_commit_or_entry_attests_nothing() {
        let install = Install::new("incomplete");
        install.manifest(&digest(b"pretend executable bytes"), "0.1.0");
        assert!(matches!(
            of_binary(&install.binary()),
            BuildIdentity::Unattested("release-manifest.json does not attest this binary")
        ));
        std::fs::write(install.0.join(MANIFEST), "{not json")
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(matches!(
            of_binary(&install.binary()),
            BuildIdentity::Unattested(_)
        ));
    }

    #[test]
    fn a_package_whose_commits_disagree_attests_nothing() {
        // The binary's own source_commit is the identity; an engine_version naming
        // another commit is the mismatch the packager refuses.
        let install = Install::new("conflict");
        install.manifest_with(
            &digest(b"pretend executable bytes"),
            "ffffffffffffffffffffffffffffffffffffffff",
            COMMIT,
        );
        assert_eq!(
            of_binary(&install.binary()),
            BuildIdentity::Unattested("release-manifest.json does not attest this binary")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_in_place_of_the_manifest_is_refused_without_blocking() {
        let install = Install::new("fifo");
        let made = std::process::Command::new("mkfifo")
            .arg(install.0.join(MANIFEST))
            .status()
            .unwrap_or_else(|error| panic!("mkfifo: {error}"));
        assert!(made.success());
        let binary = install.binary();
        // Nothing ever writes to the FIFO: a blocking open would never return,
        // so the read runs on its own thread and the test waits a bounded time.
        let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
        let filled = std::sync::Arc::clone(&slot);
        std::thread::spawn(move || {
            let identity = of_binary(&binary);
            if let Ok(mut slot) = filled.lock() {
                *slot = Some(identity);
            }
        });
        let mut identity = None;
        for _ in 0..200 {
            identity = slot.lock().ok().and_then(|mut slot| slot.take());
            if identity.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            identity.unwrap_or_else(|| panic!("reading a FIFO manifest blocked")),
            BuildIdentity::Unattested("release-manifest.json beside this binary is unreadable")
        );
    }
}
