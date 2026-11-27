//! Exact-resource normalization before preview and again before contained retry.
//! Copyright 2025 OpenAI.
//! Apache-2.0. Modified: existing native objects only, identity revalidation,
//! protected-resource exclusions and no implicit parent-directory grants.
use crate::Policy;
use same_file::Handle;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// A request target, not an authorization. The permission owner must approve
/// this exact `path()` and bind it into a fresh capability before applying it.
/// Keeping the handle alive detects replacement while the dialog is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteTarget {
    path: PathBuf,
    identity: Arc<Handle>,
}

impl WriteTarget {
    /// Resolve an existing regular file or directory for a permission preview.
    /// Creating a new file requires requesting its parent directory explicitly.
    pub fn resolve(policy: &Policy, requested: &Path) -> Result<Self, String> {
        if policy.readable_roots.len()
            + policy.writable_roots.len()
            + policy.protected_paths.len()
            + policy.denied_paths.len()
            > 128
        {
            return Err("Sandbox policy exceeds 128 paths".into());
        }
        validate_path(requested)?;
        let path = fs::canonicalize(requested).map_err(|error| {
            format!(
                "Sandbox write target {} is unavailable: {error}. For a new file, request its parent directory explicitly",
                requested.display()
            )
        })?;
        validate_path(&path)?;
        let identity = open_identity(&path)
            .map_err(|error| format!("Sandbox write target identity is unavailable: {error}"))?;
        let metadata = identity
            .as_file()
            .metadata()
            .map_err(|error| format!("Sandbox write target metadata is unavailable: {error}"))?;
        if !metadata.is_file() && !metadata.is_dir() {
            return Err("Sandbox write grants require a regular file or directory".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.is_file() && metadata.nlink() != 1 {
                return Err("Sandbox cannot grant writes through hard-link aliases".into());
            }
        }
        #[cfg(windows)]
        if metadata.is_file()
            && winapi_util::file::information(identity.as_file())
                .map_err(|error| {
                    format!("Sandbox write target link count is unavailable: {error}")
                })?
                .number_of_links()
                != 1
        {
            return Err("Sandbox cannot grant writes through hard-link aliases".into());
        }
        let target = Self {
            path,
            identity: Arc::new(identity),
        };
        target.check_protection(policy)?;
        Ok(target)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Transfer a duplicate of the pinned object over the owner's authenticated
    /// local channel. The descriptor remains close-on-exec in the sender.
    #[cfg(target_os = "linux")]
    pub fn duplicate_file(&self) -> std::io::Result<fs::File> {
        self.identity.as_file().try_clone()
    }

    /// Adopt a transferred pin only if the requested name still denotes that
    /// exact object and remains grantable under the receiver's policy. This
    /// validates the target; the permission owner still supplies authorization.
    #[cfg(target_os = "linux")]
    pub fn from_file(policy: &Policy, requested: &Path, file: fs::File) -> Result<Self, String> {
        let current = Self::resolve(policy, requested)?;
        let identity = Handle::from_file(file)
            .map_err(|error| format!("Sandbox transferred identity is unavailable: {error}"))?;
        if current.identity.as_ref() != &identity {
            return Err("Sandbox write target changed; request approval again".into());
        }
        Ok(Self {
            path: current.path,
            identity: Arc::new(identity),
        })
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn native_descriptor(&self) -> std::os::fd::BorrowedFd<'_> {
        use std::os::fd::AsFd;
        self.identity.as_file().as_fd()
    }

    fn check_protection(&self, policy: &Policy) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        if ["/proc", "/sys", "/dev"].iter().any(|system| {
            self.path.starts_with(system) || Path::new(system).starts_with(&self.path)
        }) {
            return Err("Sandbox cannot grant writes to kernel filesystems".into());
        }
        for protected in policy.protected_paths.iter().chain(&policy.denied_paths) {
            let resolved = resolve_missing(protected)?;
            if self.path.starts_with(&resolved) || resolved.starts_with(&self.path) {
                return Err(format!(
                    "Sandbox cannot grant writes overlapping protected path: {}",
                    resolved.display()
                ));
            }
            match open_identity(&resolved) {
                Ok(handle) if &handle == self.identity.as_ref() => {
                    return Err(format!(
                        "Sandbox cannot grant a protected file alias: {}",
                        self.path.display()
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "Sandbox protected identity is unavailable: {error}"
                    ));
                }
            }
        }
        Ok(())
    }
}

fn open_identity(path: &Path) -> std::io::Result<Handle> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Metadata-only: neither a replaced FIFO nor a device is opened for I/O.
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_PATH)
            .open(path)?;
        Handle::from_file(file)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        const FILE_READ_ATTRIBUTES: u32 = 0x80;
        const FILE_READ_DATA_OR_LIST_DIRECTORY: u32 = 0x1;
        const FILE_SHARE_READ_WRITE: u32 = 0x3;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        // Attribute-only access bypasses Windows sharing checks. Request
        // read-data/list-directory access without performing reads, so denying
        // delete sharing pins the name through native binding and child use.
        let file = fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES | FILE_READ_DATA_OR_LIST_DIRECTORY)
            .share_mode(FILE_SHARE_READ_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(std::io::Error::other(
                "Sandbox write target changed to a reparse point",
            ));
        }
        Handle::from_file(file)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = path;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Sandbox write grants are unavailable on this platform",
        ))
    }
}

impl Policy {
    /// Native adapters retain the targets through binding and child lifetime.
    /// Never export the amended paths as standalone authority to another owner.
    pub(crate) fn with_write_targets(&self, targets: &[WriteTarget]) -> Result<Self, String> {
        if targets.len() > 128 {
            return Err("Sandbox write targets exceed 128 paths".into());
        }
        let mut policy = self.clone();
        for target in targets {
            let current = WriteTarget::resolve(self, target.path())?;
            if current.path != target.path || current.identity != target.identity {
                return Err("Sandbox write target changed; request approval again".into());
            }
            if policy.writable_roots.contains(&target.path) {
                continue;
            }
            if policy.readable_roots.len()
                + policy.writable_roots.len()
                + policy.protected_paths.len()
                + policy.denied_paths.len()
                >= 128
            {
                return Err("Sandbox policy exceeds 128 paths".into());
            }
            policy.writable_roots.push(target.path.clone());
        }
        Ok(policy)
    }
}

fn validate_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path.components().count() >= 128
        || path.components().any(|c| matches!(c, Component::ParentDir))
        || path
            .to_str()
            .is_none_or(|s| s.len() > 32 * 1024 || s.contains('\0'))
    {
        return Err(
            "Sandbox write target needs an absolute Unicode path without parent traversal".into(),
        );
    }
    Ok(())
}

fn resolve_missing(path: &Path) -> Result<PathBuf, String> {
    validate_path(path)?;
    let mut ancestor = path;
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => {
                let mut resolved = fs::canonicalize(ancestor)
                    .map_err(|error| format!("Sandbox protected path is unavailable: {error}"))?;
                for name in missing.iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    ancestor
                        .file_name()
                        .ok_or("Sandbox protected path has no existing root")?,
                );
                ancestor = ancestor
                    .parent()
                    .ok_or("Sandbox protected path has no parent")?;
            }
            Err(error) => return Err(format!("Sandbox protected path is unavailable: {error}")),
        }
    }
}
