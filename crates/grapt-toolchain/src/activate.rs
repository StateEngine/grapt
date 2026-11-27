//! Staged, atomic activation with re-cut refusal and identity-write rollback.
//!
//! The ordering that makes a crash recoverable: staging is untrusted and
//! disposable (the private bounded staging root is cleared however the
//! attempt ends, including a crashed one); the archive is decoded IN MEMORY
//! and never staged as a file; the canonical binary is published by ONE
//! rename with the previous executable retained as a hard link to
//! `.last-known-good` before that rename, so a crash in the publish window
//! leaves the previous executable or the new one at the canonical path,
//! never neither; and the identity record is published LAST, so a failure
//! there rolls the activation back rather than leaving an unattributable
//! executable at the canonical path.

use std::path::{Component, Path, PathBuf};

use grapt_types::ClockAuthority;

use crate::archive::{decode_tar, BINARY_MAX_BYTES};
use crate::identity::{InstalledIdentity, IDENTITY_MAX_BYTES};
use crate::pin::{attribute, PinTable, PIN_MAX_BYTES};
use crate::refusal::{Refusal, RefusalReason};
use crate::source::ArchiveSource;
use crate::value::{Sha256, TargetTriple};

/// The receipt byte bound: 16 KiB.
pub const RECEIPT_MAX_BYTES: usize = 16384;

/// The fixed staged layout. Every path the boundary touches is derived from
/// these relative names; nothing outside them is read or written.
pub const LAYOUT: Layout = Layout;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout;

impl Layout {
    fn tools(&self, root: &Path) -> PathBuf {
        root.join("tools")
    }

    pub fn tool_dir(&self, root: &Path, tool: &str) -> PathBuf {
        self.tools(root).join(tool)
    }

    pub fn bin_dir(&self, root: &Path, tool: &str) -> PathBuf {
        self.tool_dir(root, tool).join("bin")
    }

    pub fn canonical(&self, root: &Path, tool: &str, executable: &str) -> PathBuf {
        self.bin_dir(root, tool).join(executable)
    }

    pub fn staged(&self, root: &Path, tool: &str, executable: &str) -> PathBuf {
        self.bin_dir(root, tool)
            .join(format!("{executable}.staged"))
    }

    pub fn last_known_good(&self, root: &Path, tool: &str, executable: &str) -> PathBuf {
        self.bin_dir(root, tool)
            .join(format!("{executable}.last-known-good"))
    }

    pub fn identity(&self, root: &Path, tool: &str) -> PathBuf {
        self.tool_dir(root, tool).join("identity.json")
    }

    pub fn staging_dir(&self, root: &Path, tool: &str) -> PathBuf {
        self.tool_dir(root, tool).join(".staging")
    }

    pub fn lock(&self, root: &Path, tool: &str) -> PathBuf {
        self.tool_dir(root, tool).join(".activation.lock")
    }

    /// The canonical path relative to the activation root: the layout is
    /// fixed, so a relative name leaks no operator tree.
    pub fn canonical_relative(&self, tool: &str, executable: &str) -> String {
        format!("tools/{tool}/bin/{executable}")
    }
}

/// One activation attempt, fully described: nothing is read from the
/// environment beyond what is named here.
#[derive(Debug)]
pub struct ActivationRequest<'a> {
    /// The activation root: the directory that owns the pin, the sources and
    /// the tools/ tree. It must exist as a directory.
    pub root: PathBuf,
    /// The pin file, relative to the root, tracked at HEAD of the root's git
    /// worktree.
    pub pin: PathBuf,
    /// The requested target. An absent pin entry is `target-unsupported`;
    /// there is no default target.
    pub target: TargetTriple,
    /// The identity record's platform fact ("windows", "linux", "macos").
    pub platform: String,
    /// The identity record's architecture fact ("x86_64", "aarch64", ...).
    pub arch: String,
    /// The injected clock: `installed_at_ms` comes from here, never ambient.
    pub clock: &'a dyn ClockAuthority,
    /// The deterministic source adapter.
    pub source: &'a dyn ArchiveSource,
}

/// The success receipt: the activation or reuse that just happened, bounded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationReceipt {
    /// "activated" or "reused".
    pub outcome: &'static str,
    pub tool: String,
    pub version: String,
    pub target: String,
    pub source: String,
    pub archive_sha256: String,
    pub binary_sha256: String,
    pub installed_at_ms: u64,
    /// The canonical executable relative to the activation root.
    pub executable: String,
    /// The commit the pin was attributed to.
    pub commit: String,
}

impl ActivationReceipt {
    pub const MAX_BYTES: usize = RECEIPT_MAX_BYTES;

    /// The bounded receipt JSON (schema `grapt.toolchain.receipt.v1`).
    pub fn json(&self) -> String {
        let value = serde_json::json!({
            "schema": "grapt.toolchain.receipt.v1",
            "outcome": self.outcome,
            "tool": self.tool,
            "version": self.version,
            "target": self.target,
            "source": self.source,
            "archive_sha256": self.archive_sha256,
            "binary_sha256": self.binary_sha256,
            "installed_at_ms": self.installed_at_ms,
            "executable": self.executable,
            "commit": self.commit,
        });
        let text = value.to_string();
        if text.len() <= Self::MAX_BYTES {
            text
        } else {
            // The bound is declared in the contract; a receipt this large is
            // a defect, and the receipt must still be bounded.
            serde_json::json!({
                "schema": "grapt.toolchain.receipt.v1",
                "outcome": self.outcome,
                "tool": self.tool,
            })
            .to_string()
        }
    }
}

/// Re-exported from `refusal` so the boundary's public surface is one
/// `use`: the context every refusal carries.
pub use crate::refusal::RefusalContext;

/// The exclusive activation lock: atomic create-new, held for the whole
/// attempt, released however the attempt ends. A held lock names its holder
/// process so an operator can confirm a crashed holder is gone before
/// removing a stale lock; the boundary itself never guesses.
fn take_lock(path: &Path, clock: &dyn ClockAuthority) -> Result<(), Refusal> {
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => file,
        Err(_) => {
            let detail = match std::fs::read_to_string(path) {
                Ok(text) => {
                    let value: serde_json::Value =
                        serde_json::from_str(&text).unwrap_or_default();
                    match value.get("holder_pid").and_then(|pid| pid.as_u64()) {
                        Some(pid) => format!(
                            "another activation holds the lock (holder process {pid}); confirm it is gone before removing the lock file"
                        ),
                        None => {
                        "another activation holds the lock; confirm the holder is gone before removing the lock file".to_owned()
                    },
                    }
                }
                Err(_) => {
                    "another activation holds the lock; confirm the holder is gone before removing the lock file".to_owned()
                }
            };
            return Err(Refusal::new(RefusalReason::ActivationInFlight, detail));
        }
    };
    let text = serde_json::json!({
        "schema": "grapt.toolchain.lock.v1",
        "holder_pid": std::process::id(),
        "acquired_at_ms": clock.now().as_millis(),
    })
    .to_string();
    match std::io::Write::write_all(&mut file, text.as_bytes()) {
        Ok(()) => Ok(()),
        Err(_) => {
            let _ = std::fs::remove_file(path);
            Err(Refusal::new(
                RefusalReason::ActivationInFlight,
                "the lock could not be established; the attempt was not taken",
            ))
        }
    }
}

/// Clears the lock and the untrusted staging however this attempt ends.
struct AttemptGuard {
    lock: Option<PathBuf>,
    staging: Option<PathBuf>,
}

impl AttemptGuard {
    fn new() -> Self {
        Self {
            lock: None,
            staging: None,
        }
    }
}

impl Drop for AttemptGuard {
    fn drop(&mut self) {
        if let Some(lock) = self.lock.take() {
            let _ = std::fs::remove_file(lock);
        }
        if let Some(staging) = self.staging.take() {
            let _ = std::fs::remove_dir_all(staging);
        }
    }
}

/// Read a file under a byte bound: the whole read, then the bound. A
/// missing or unreadable file is `None` (the caller decides whether
/// absence refuses or merely fails to vouch); an over-bound file is
/// `None` too, and a directory that opens for read fails the read the
/// same way. The bound is the law: no caller reads this boundary's files
/// without one.
fn read_bounded(path: &Path, max: usize) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut file = std::fs::File::open(path).ok()?;
    let mut buffer = [0u8; 65536];
    loop {
        let read = std::io::Read::read(&mut file, &mut buffer).ok()?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > max {
            return None;
        }
    }
    Some(bytes)
}

/// Read the identity record beside the canonical executable. An undecodable
/// record is never a vouch: it does not refuse the attempt, it simply does
/// not count, and the activation re-acquires.
fn read_identity(path: &Path) -> Option<InstalledIdentity> {
    let bytes = read_bounded(path, IDENTITY_MAX_BYTES)?;
    InstalledIdentity::decode(&bytes).ok()
}

/// Activate the pinned tool for the requested target through the full law:
/// root, pin data, pin source, pin decode, target, lock, reuse check,
/// fetch, digest, staging, decode, publish, identity.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub fn activate(request: &ActivationRequest) -> Result<ActivationReceipt, Refusal> {
    let layout = LAYOUT;
    let root = request.root.as_path();

    // 1. The root: a missing root and an unreadable tree are distinct facts.
    let root_is_dir = std::fs::metadata(root)
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false);
    if !root_is_dir {
        return Err(Refusal::new(
            RefusalReason::RootMissing,
            "the activation root does not exist or is not a directory",
        ));
    }

    // 2. The pin file: the path shape first, then missing, unreadable and
    //    oversized stay apart.
    for component in request.pin.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(Refusal::new(
                RefusalReason::PinUnreadable,
                "the pin path must be a relative path under the activation root",
            ));
        }
    }
    let pin_path = root.join(&request.pin);
    let pin_bytes = match std::fs::metadata(&pin_path) {
        Ok(metadata) if metadata.is_file() => {
            if metadata.len() > PIN_MAX_BYTES as u64 {
                return Err(Refusal::new(
                    RefusalReason::PinOversized,
                    "the pin file exceeds the 64 KiB bound",
                ));
            }
            match read_bounded(&pin_path, PIN_MAX_BYTES) {
                Some(bytes) => bytes,
                None => {
                    return Err(Refusal::new(
                        RefusalReason::PinUnreadable,
                        "the pin file cannot be read",
                    ))
                }
            }
        }
        Ok(_) => {
            return Err(Refusal::new(
                RefusalReason::PinUnreadable,
                "the pin file exists but is not a regular file",
            ))
        }
        Err(_) => {
            let missing = !std::fs::symlink_metadata(&pin_path).is_ok();
            return if missing {
                Err(Refusal::new(
                    RefusalReason::PinMissing,
                    "the pin file does not exist",
                ))
            } else {
                Err(Refusal::new(
                    RefusalReason::PinUnreadable,
                    "the pin file exists but is not a regular file",
                ))
            };
        }
    };

    // 3. The pin's SOURCE, before its contents: an unattributable pin is a
    //    workspace-state problem and is never silently read.
    let attribution = attribute(root, &request.pin, &pin_bytes)?;

    // 4. The pin's CONTENTS: strict decode, then the target entry.
    let table = PinTable::decode(&pin_bytes)?;
    let pin_entry = match table.pin(&request.target) {
        Some(entry) => entry,
        None => {
            return Err(Refusal::new(
                RefusalReason::TargetUnsupported,
                "the pin table has no entry for the requested target; there is no default",
            )
            .with_tool(table.tool().as_str())
            .with_version(table.version().as_str())
            .with_target(request.target.as_str()))
        }
    };
    let context = |refusal: Refusal| {
        refusal
            .with_tool(table.tool().as_str())
            .with_version(table.version().as_str())
            .with_target(request.target.as_str())
            .with_source(pin_entry.source.as_str())
            .with_archive_sha256(&pin_entry.archive_sha256.hex())
    };
    let tool = table.tool().as_str();
    let executable = pin_entry.entry.file_name();

    // 5. The tool tree and the exclusive lock.
    let tool_dir = layout.tool_dir(root, tool);
    if std::fs::create_dir_all(&tool_dir).is_err() {
        return Err(Refusal::new(
            RefusalReason::RootUnreadable,
            "the tool directory tree cannot be created",
        ));
    }
    let lock_path = layout.lock(root, tool);
    take_lock(&lock_path, request.clock)?;
    let mut guard = AttemptGuard::new();
    guard.lock = Some(lock_path);

    // 6. The working tree: bin and the private bounded staging root.
    //    Staging left by a crashed attempt is UNTRUSTED and is cleared.
    if std::fs::create_dir_all(layout.bin_dir(root, tool)).is_err() {
        return Err(context(Refusal::new(
            RefusalReason::RootUnreadable,
            "the tool directory tree cannot be created",
        )));
    }
    let staging_dir = layout.staging_dir(root, tool);
    match std::fs::symlink_metadata(&staging_dir) {
        Ok(metadata) if metadata.is_dir() => {
            if std::fs::remove_dir_all(&staging_dir).is_err() {
                return Err(context(Refusal::new(
                    RefusalReason::RootUnreadable,
                    "the private staging root cannot be cleared",
                )));
            }
        }
        Ok(_) => {
            if std::fs::remove_file(&staging_dir).is_err() {
                return Err(context(Refusal::new(
                    RefusalReason::RootUnreadable,
                    "the private staging root cannot be cleared",
                )));
            }
        }
        Err(_) => {}
    }
    if std::fs::create_dir_all(&staging_dir).is_err() {
        return Err(context(Refusal::new(
            RefusalReason::RootUnreadable,
            "the private staging root cannot be created",
        )));
    }
    guard.staging = Some(staging_dir.clone());
    let staged_binary = layout.staged(root, tool, executable);
    let _ = std::fs::remove_file(&staged_binary); // untrusted staging residue

    // 7. The reuse law: an existing activation is reused only when its
    //    record decodes strictly, matches the CURRENT pin, and the canonical
    //    bytes still hash to the record's binary digest.
    let canonical = layout.canonical(root, tool, executable);
    let identity_path = layout.identity(root, tool);
    if let Some(identity) = read_identity(&identity_path) {
        if identity.matches(pin_entry, &request.platform, &request.arch) {
            if let Some(bytes) = read_bounded(&canonical, BINARY_MAX_BYTES) {
                if Sha256::of(&bytes) == identity.binary_sha256 {
                    return Ok(ActivationReceipt {
                        outcome: "reused",
                        tool: tool.to_owned(),
                        version: table.version().as_str().to_owned(),
                        target: request.target.as_str().to_owned(),
                        source: pin_entry.source.as_str().to_owned(),
                        archive_sha256: pin_entry.archive_sha256.hex(),
                        binary_sha256: identity.binary_sha256.hex(),
                        installed_at_ms: identity.installed_at_ms,
                        executable: layout.canonical_relative(tool, executable),
                        commit: attribution.commit.clone(),
                    });
                }
            }
        }
    }

    // 8. Acquire: the deterministic adapter, then the archive digest.
    let archive_bytes = request.source.fetch(&pin_entry.source).map_err(context)?;
    let actual_archive = Sha256::of(&archive_bytes);
    if actual_archive != pin_entry.archive_sha256 {
        return Err(context(
            Refusal::new(
                RefusalReason::ArchiveDigestMismatch,
                "the delivered archive does not hash to the pinned archive digest",
            )
            .with_digests(&pin_entry.archive_sha256.hex(), &actual_archive.hex()),
        ));
    }
    // 9. Decode the verified archive under the bounds, in memory, then the
    //    entry: the archive itself never stands as a file.
    let decoded = decode_tar(&archive_bytes).map_err(context)?;
    let binary_bytes = match decoded.entry(&pin_entry.entry) {
        Some(bytes) => bytes,
        None => {
            return Err(context(Refusal::new(
                RefusalReason::ArchiveMissingBinary,
                "the verified archive does not contain the pinned entry",
            )))
        }
    };
    if binary_bytes.len() > BINARY_MAX_BYTES {
        return Err(context(Refusal::new(
            RefusalReason::ArchiveUnsafeEntry,
            "the pinned binary exceeds the 8 MiB bound",
        )));
    }
    let expected_binary = Sha256::of(binary_bytes);

    // 10. Stage the binary beside the canonical name, then prove the staged
    //     bytes ARE the verified bytes before anything may execute them.
    if std::fs::write(&staged_binary, binary_bytes).is_err() {
        return Err(context(Refusal::new(
            RefusalReason::ActivationFailed,
            "the verified binary could not be staged",
        )));
    }
    if let Err(refusal) = verify_staged_copy(&staged_binary, expected_binary) {
        return Err(context(refusal));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The executable bit is a unix fact; best-effort, like the oracle's
        // observable behavior — the digest law is the binding one.
        let _ = std::fs::set_permissions(&staged_binary, std::fs::Permissions::from_mode(0o755));
    }

    // 11. Retain the previous executable as last-known-good, then publish.
    //     The publish is ONE rename; a crash anywhere in the window leaves
    //     the previous executable or the new one at the canonical path,
    //     never neither.
    let had_previous =
        publish_staged_binary(&layout, root, tool, executable, &staged_binary).map_err(context)?;

    // 12. The identity record: staged, re-verified, renamed LAST. If it
    //     cannot be published, this run does not get to execute the binary:
    //     roll the activation back so an unattributable executable is never
    //     left at the canonical path.
    let identity = InstalledIdentity {
        tool: table.tool().clone(),
        version: table.version().clone(),
        platform: request.platform.clone(),
        arch: request.arch.clone(),
        target: request.target.clone(),
        source: pin_entry.source.clone(),
        archive_sha256: pin_entry.archive_sha256,
        binary_sha256: expected_binary,
        installed_at_ms: {
            let millis = request.clock.now().as_millis();
            if millis < 0 {
                0
            } else {
                millis as u64
            }
        },
    };
    let staged_identity = staging_dir.join("identity.json");
    let identity_published =
        match persist_identity_record(&identity, &staged_identity, &identity_path) {
            Ok(published) => published,
            Err(refusal) => {
                rollback_identity_write(&guard, &layout, root, tool, executable, had_previous);
                return Err(context(refusal));
            }
        };
    if !identity_published {
        rollback_identity_write(&guard, &layout, root, tool, executable, had_previous);
        return Err(context(Refusal::new(
            RefusalReason::IdentityWriteFailed,
            "the identity record could not be published; the activation was rolled back rather than run an unattributable binary",
        )));
    }

    // 13. Success: the guard's drop releases the lock and clears staging.
    Ok(ActivationReceipt {
        outcome: "activated",
        tool: tool.to_owned(),
        version: table.version().as_str().to_owned(),
        target: request.target.as_str().to_owned(),
        source: pin_entry.source.as_str().to_owned(),
        archive_sha256: pin_entry.archive_sha256.hex(),
        binary_sha256: identity.binary_sha256.hex(),
        installed_at_ms: identity.installed_at_ms,
        executable: layout.canonical_relative(tool, executable),
        commit: attribution.commit,
    })
}

/// The publish: retain the previous executable as a hard link to
/// last-known-good (so it survives however the publish ends), clear any
/// orphan last-known-good, and rename the staged binary to the canonical
/// path. The rename is the publish: before it, the canonical path stands
/// as it stood; after it, the staged bytes stand there. Failure removes
/// the staged file and refuses; the canonical path is untouched either
/// way.
fn publish_staged_binary(
    layout: &Layout,
    root: &Path,
    tool: &str,
    executable: &str,
    staged_binary: &Path,
) -> Result<bool, Refusal> {
    let canonical = layout.canonical(root, tool, executable);
    let last_good = layout.last_known_good(root, tool, executable);
    let had_previous = std::fs::metadata(&canonical)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false);
    // Unconditional: an orphan last-known-good from a crashed publish is
    // residue, and the publish that takes is the moment it goes.
    let _ = std::fs::remove_file(&last_good);
    if had_previous && std::fs::hard_link(&canonical, &last_good).is_err() {
        let _ = std::fs::remove_file(staged_binary);
        return Err(Refusal::new(
            RefusalReason::ActivationFailed,
            "the previous executable could not be retained; the activation was not taken",
        ));
    }
    if std::fs::rename(staged_binary, &canonical).is_err() {
        let _ = std::fs::remove_file(staged_binary);
        return Err(Refusal::new(
            RefusalReason::ActivationFailed,
            "the staged executable could not be published; the activation was not taken",
        ));
    }
    Ok(had_previous)
}

/// The identity-record publish: encode under the bound, stage, re-read,
/// re-decode, require the decoded record to equal the one the run vouches,
/// and rename. Any step that cannot be proven is a failure: the caller
/// rolls the activation back rather than run an unattributable binary. The
/// encode failures carry no context here; the caller applies it, so every
/// refusal names the same facts.
fn persist_identity_record(
    identity: &InstalledIdentity,
    staged_identity: &Path,
    identity_path: &Path,
) -> Result<bool, Refusal> {
    let identity_text = identity.encode().map_err(|_| {
        Refusal::new(
            RefusalReason::IdentityWriteFailed,
            "the identity record could not be encoded; the activation was rolled back",
        )
    })?;
    if identity_text.len() > IDENTITY_MAX_BYTES {
        return Err(Refusal::new(
            RefusalReason::IdentityWriteFailed,
            "the identity record exceeds the 4 KiB bound; the activation was rolled back",
        ));
    }
    let published = match std::fs::write(staged_identity, identity_text.as_bytes()) {
        Ok(()) => match std::fs::read(staged_identity) {
            Ok(bytes) => match InstalledIdentity::decode(&bytes) {
                Ok(decoded) if decoded == *identity => {
                    std::fs::rename(staged_identity, identity_path).is_ok()
                }
                Ok(_) => false,
                Err(_) => false,
            },
            Err(_) => false,
        },
        Err(_) => false,
    };
    Ok(published)
}

/// R19: prove the staged bytes ARE the verified bytes. A staged copy that
/// reports success but re-reads to a different digest — or that cannot be
/// re-read at all — is refused, and the staged file is removed so nothing
/// unproven can later be renamed to the canonical path. The caller applies
/// the refusal context; the wire behavior is unchanged by the extraction.
fn verify_staged_copy(path: &Path, expected: Sha256) -> Result<(), Refusal> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let staged = Sha256::of(&bytes);
            if staged == expected {
                return Ok(());
            }
            let _ = std::fs::remove_file(path);
            Err(Refusal::new(
                RefusalReason::StagedCopyIncomplete,
                "a copy that reported success did not land complete",
            )
            .with_digests(&expected.hex(), &staged.hex()))
        }
        Err(_) => {
            let _ = std::fs::remove_file(path);
            Err(Refusal::new(
                RefusalReason::StagedCopyIncomplete,
                "the staged executable could not be re-read",
            ))
        }
    }
}
/// The identity-write rollback: the new binary is removed and the previous
/// executable is restored (the record still on disk describes it, so the
/// pair stays consistent), or nothing is left when there was no previous.
fn rollback_identity_write(
    guard: &AttemptGuard,
    layout: &Layout,
    root: &Path,
    tool: &str,
    executable: &str,
    had_previous: bool,
) {
    let canonical = layout.canonical(root, tool, executable);
    let _ = std::fs::remove_file(&canonical);
    if had_previous {
        let _ = std::fs::rename(layout.last_known_good(root, tool, executable), &canonical);
    }
    // The staged identity is cleared with the staging root by the guard.
    let _ = guard;
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "grapt-toolchain-{}-staged-{label}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir)
            .unwrap_or_else(|error| panic!("test: create temp dir: {error}"));
        dir
    }

    #[test]
    fn a_staged_copy_that_does_not_rehash_refuses_and_is_removed() {
        let dir = temp_dir("mismatch");
        let path = dir.join("staged");
        std::fs::write(&path, b"what-actually-landed")
            .unwrap_or_else(|error| panic!("test: write staged: {error}"));
        let expected = Sha256::of(b"what-was-verified");
        let refusal = match verify_staged_copy(&path, expected) {
            Ok(()) => panic!("test: a mismatched staged copy must refuse"),
            Err(refusal) => refusal,
        };
        assert_eq!(refusal.reason, RefusalReason::StagedCopyIncomplete);
        assert_eq!(
            refusal.context.expected_sha256.as_deref(),
            Some(expected.hex().as_str()),
            "the refusal names the verified digest"
        );
        assert_ne!(
            refusal.context.actual_sha256.as_deref(),
            Some(expected.hex().as_str()),
            "the staged digest is named, and it is not the verified one"
        );
        assert!(!path.exists(), "the unproven staged file is removed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_staged_copy_that_cannot_be_re_read_refuses() {
        let dir = temp_dir("unreadable");
        let path = dir.join("never-written");
        let expected = Sha256::of(b"what-was-verified");
        let refusal = match verify_staged_copy(&path, expected) {
            Ok(()) => panic!("test: an unreadable staged copy must refuse"),
            Err(refusal) => refusal,
        };
        assert_eq!(refusal.reason, RefusalReason::StagedCopyIncomplete);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
