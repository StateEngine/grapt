//! A locked, claimable sibling stage; a killed owner cannot block the next write.
use super::{recheck_parent, set_mode};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

/// The product name in stage file names written before the rename.
const LEGACY_STAGE: &str = "secode"; // grapt-rename: keep

/// The open identity owns the lock until publication or cleanup finishes.
struct Claim {
    identity: same_file::Handle,
}

impl Claim {
    fn lock(file: File, path: &Path) -> Result<Self, String> {
        let identity = same_file::Handle::from_file(file).map_err(|error| error.to_string())?;
        identity.as_file().try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => "another writer owns this stage".to_owned(),
            std::fs::TryLockError::Error(error) => format!("stage lock unavailable: {error}"),
        })?;
        let claim = Self { identity };
        claim.check_path(path)?;
        Ok(claim)
    }

    fn check_path(&self, path: &Path) -> Result<(), String> {
        // Validate the opened object, not metadata observed before another
        // path open: a replacement FIFO must never block commit or Drop.
        let file = open_stage(path).map_err(|error| error.to_string())?;
        let current = same_file::Handle::from_file(file).map_err(|error| error.to_string())?;
        if self.identity != current {
            return Err("stage identity changed; file retained".into());
        }
        Ok(())
    }

    fn create(path: &Path) -> Result<Self, String> {
        // Competing reclaimers can race an unlink/create. Never wait, scan the
        // directory or retry indefinitely, and never truncate an existing file.
        for _ in 0..16 {
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(file) => return Self::lock(file, path),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => Self::reclaim(path)?,
                Err(error) => return Err(error.to_string()),
            }
        }
        Err("stage remained contested after 16 claims".into())
    }

    fn reclaim(path: &Path) -> Result<(), String> {
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
            Ok(metadata) => regular_stage(&metadata)?,
        }
        let file = match open_stage(path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        let claim = Self::lock(file, path)?;
        // A live stage holds this lock. A killed owner releases it in the
        // kernel; checking the SAME name prevents an old open handle from
        // reclaiming a successor's file after the predecessor renamed away.
        claim.check_path(path)?;
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("stale stage could not be reclaimed: {error}")),
        }
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        let _unlock = self.identity.as_file().unlock();
    }
}

/// Every inspection uses this same bounded, no-follow open. Metadata on the
/// returned handle also rejects a nonregular entry substituted before open.
pub(in crate::dogfood::tools) fn open_stage(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    regular_stage(&file.metadata()?).map_err(std::io::Error::other)?;
    Ok(file)
}

fn regular_stage(metadata: &std::fs::Metadata) -> Result<(), String> {
    let linked = metadata.file_type().is_symlink();
    #[cfg(windows)]
    let linked = {
        use std::os::windows::fs::MetadataExt;
        linked || metadata.file_attributes() & 0x400 != 0
    };
    if !metadata.is_file() || linked {
        return Err("stage is not a regular file; entry retained".into());
    }
    Ok(())
}

/// Replace `path` with `content` through a temporary file beside it, keeping
/// the file's permission bits: [`stage`], then [`Staged::commit`]. The rename
/// swaps the directory entry, so a hard link to the old file keeps the old
/// content.
pub(in crate::dogfood::tools) fn atomic_replace(
    root: &Path,
    path: &Path,
    content: &[u8],
    owner: &str,
    tag: &str,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    stage(root, path, content, owner, tag)?.commit(check)
}

/// A replacement written and synced beside its target, not yet in place. If
/// it is dropped without committing, its temporary file is removed.
pub(in crate::dogfood::tools) struct Staged {
    temp: PathBuf,
    path: PathBuf,
    owner: String,
    committed: bool,
    claim: Claim,
}

/// Write `content` to a temporary file beside `path` with the target's
/// permission bits, and sync it. The target is not touched.
pub(in crate::dogfood::tools) fn stage(
    root: &Path,
    path: &Path,
    content: &[u8],
    owner: &str,
    tag: &str,
) -> Result<Staged, String> {
    let permissions = std::fs::metadata(path)
        .map_err(|error| format!("{owner} source metadata changed: {error}"))?
        .permissions();
    stage_permissions(root, path, content, owner, tag, permissions)
}

pub(in crate::dogfood::tools) fn stage_with_mode(
    root: &Path,
    path: &Path,
    content: &[u8],
    owner: &str,
    tag: &str,
    mode: u32,
) -> Result<Staged, String> {
    let mut permissions = std::fs::metadata(path)
        .map_err(|error| error.to_string())?
        .permissions();
    set_mode(&mut permissions, mode)?;
    stage_permissions(root, path, content, owner, tag, permissions)
}

fn stage_permissions(
    root: &Path,
    path: &Path,
    content: &[u8],
    owner: &str,
    tag: &str,
    permissions: std::fs::Permissions,
) -> Result<Staged, String> {
    recheck_parent(root, path)?;
    let name = path
        .file_name()
        .ok_or_else(|| format!("{owner} target has no filename"))?
        .to_string_lossy();
    let legacy = path.with_file_name(format!(".{name}.grapt-{tag}.tmp"));
    // An older writer never checked its stage's identity before renaming.
    // Do not reuse that name: it must not publish this protocol's bytes.
    let temp = path.with_file_name(format!(".{name}.grapt-{tag}.v2.tmp"));
    // Writers from before the rename staged under the legacy product name, in
    // both protocols. Their leftovers are reclaimed; new stages never use it.
    let renamed = [
        path.with_file_name(format!(".{name}.{LEGACY_STAGE}-{tag}.tmp")),
        path.with_file_name(format!(".{name}.{LEGACY_STAGE}-{tag}.v2.tmp")),
    ];
    let claim = Claim::create(&temp).map_err(|error| {
        format!(
            "{owner} temporary file unavailable ({}): {error}",
            temp.display()
        )
    })?;
    let mut staged = Staged {
        temp,
        path: path.to_path_buf(),
        owner: owner.to_owned(),
        committed: false,
        claim,
    };
    Claim::reclaim(&legacy)?;
    for stale in &renamed {
        Claim::reclaim(stale)?;
    }
    let file = staged.claim.identity.as_file_mut();
    file.write_all(content)
        .and_then(|()| file.set_permissions(permissions))
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("{owner} temporary write failed: {error}"))?;
    Ok(staged)
}

impl Staged {
    /// The staged temporary file.
    #[cfg(test)]
    pub(in crate::dogfood::tools) fn temp(&self) -> &Path {
        &self.temp
    }

    /// Run `check` now, after staging and immediately before the rename, then
    /// rename into place. A target changed while staging is refused; only the
    /// rename itself is not covered, as no portable filesystem
    /// compare-and-swap exists. Any failure removes the temporary file.
    pub(in crate::dogfood::tools) fn commit(
        mut self,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        self.claim.check_path(&self.temp)?;
        std::fs::rename(&self.temp, &self.path).map_err(|error| {
            format!(
                "{} replacement failed; original retained: {error}",
                self.owner
            )
        })?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.committed && self.claim.check_path(&self.temp).is_ok() {
            let _cleanup = std::fs::remove_file(&self.temp);
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
