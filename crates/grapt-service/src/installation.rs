//! Install-prefix exclusion, separate from service election and liveness.
//!
//! This adapter owns only a kernel file-lock guard. It does not read or write
//! install state, select a version, elect a service or inspect a process PID.
//! A product holds a shared guard itself, so CLI-parent death cannot release
//! update exclusion while that product lives. Kernel handle closure releases
//! the lock on process death.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;

#[cfg(windows)]
mod windows;

const START_ATTEMPTS: usize = 100;
const START_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Error)]
pub enum PrefixError {
    #[error("installation prefix input is not a regular file or real directory")]
    Nonregular,
    #[error("installation prefix input changed while it was opened")]
    Changed,
    #[error("installation prefix is writable by other users; remove group/other write access before using it")]
    Writable,
    #[error("installation prefix I/O refused: {0:?}")]
    Io(std::io::ErrorKind),
    #[error(
        "installation is in use; quit grapt code and stop its services before update or revert"
    )]
    Busy,
    #[error("installation transition did not finish within 5 seconds; retry after it finishes")]
    StartBound,
}

#[derive(Debug)]
pub struct PrefixGuard {
    // Private to this owner. Actual prefix users open and lock independently;
    // incidental pre-exec copies do not own another guard's lifetime.
    _file: File,
    #[cfg(windows)]
    _directories: Vec<File>,
    #[cfg(windows)]
    _identity: same_file::Handle,
}

#[cfg(target_os = "linux")]
impl Drop for PrefixGuard {
    fn drop(&mut self) {
        use std::io::Write;

        // Unlock this owner's file description first. Closing the field after
        // Drop alone could leave its lock on an incidental pre-exec fork copy.
        if let Err(error) = self._file.unlock() {
            let _logged = writeln!(
                std::io::stderr(),
                "installation lock release failed: {:?}; descriptor will close",
                error.kind()
            );
        }
        // The private File still closes after this body, including on failure.
    }
}

fn io(error: std::io::Error) -> PrefixError {
    PrefixError::Io(error.kind())
}

#[cfg(not(windows))]
fn directory(path: &Path) -> Result<(), PrefixError> {
    if path.components().count() > 64 {
        return Err(PrefixError::Nonregular);
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        let metadata = std::fs::symlink_metadata(&current).map_err(io)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(PrefixError::Nonregular);
        }
    }
    if current.as_os_str().is_empty() {
        return Err(PrefixError::Nonregular);
    }
    Ok(())
}

#[cfg(windows)]
fn directory(path: &Path) -> Result<(), PrefixError> {
    windows::directories(path).map(|_| ())
}

fn linked(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        windows::reparse(metadata)
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn lock_file(prefix: &Path) -> Result<File, PrefixError> {
    directory(prefix)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // This per-user prefix must exclude other users from replacing the
        // named lock while a product holds its original descriptor.
        if std::fs::symlink_metadata(prefix)
            .map_err(io)?
            .permissions()
            .mode()
            & 0o022
            != 0
        {
            return Err(PrefixError::Writable);
        }
    }
    let path = prefix.join("lock");
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.is_file() || linked(&metadata) => {
            return Err(PrefixError::Nonregular);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io(error)),
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(windows)]
    let before = windows::optional_identity(&path)?;
    #[cfg(windows)]
    windows::lock_options(&mut options);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .mode(0o600);
    }
    directory(prefix)?;
    let file = options.open(&path).map_err(io)?;
    let opened = file.metadata().map_err(io)?;
    let named = std::fs::symlink_metadata(&path).map_err(io)?;
    if !opened.is_file() || linked(&opened) || !named.is_file() || linked(&named) {
        return Err(PrefixError::Nonregular);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if opened.dev() != named.dev() || opened.ino() != named.ino() || opened.nlink() != 1 {
            return Err(PrefixError::Changed);
        }
    }
    #[cfg(windows)]
    windows::identified(&file, &path, before.as_ref())?;
    Ok(file)
}

#[cfg(not(windows))]
fn identified(file: &File, prefix: &Path) -> Result<(), PrefixError> {
    directory(prefix)?;
    let opened = file.metadata().map_err(io)?;
    let named = std::fs::symlink_metadata(prefix.join("lock")).map_err(io)?;
    if !named.is_file() || linked(&named) {
        return Err(PrefixError::Nonregular);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if opened.dev() != named.dev() || opened.ino() != named.ino() || opened.nlink() != 1 {
            return Err(PrefixError::Changed);
        }
    }
    #[cfg(not(unix))]
    let _ = opened;
    Ok(())
}

impl PrefixGuard {
    /// Update/revert: refuse immediately when any prefix user holds the lock.
    pub fn exclusive(prefix: &Path) -> Result<Self, PrefixError> {
        #[cfg(windows)]
        let directories = windows::directories(prefix)?;
        let file = lock_file(prefix)?;
        #[cfg(windows)]
        let identity = windows::identity(&file)?;
        match file.try_lock() {
            Ok(()) => {
                #[cfg(not(windows))]
                identified(&file, prefix)?;
                #[cfg(windows)]
                windows::revalidate(&identity, &prefix.join("lock"))?;
                Ok(Self {
                    _file: file,
                    #[cfg(windows)]
                    _directories: directories,
                    #[cfg(windows)]
                    _identity: identity,
                })
            }
            Err(std::fs::TryLockError::WouldBlock) => Err(PrefixError::Busy),
            Err(std::fs::TryLockError::Error(error)) => Err(io(error)),
        }
    }

    /// Start: allow a transition to finish, within a fixed five-second bound.
    pub fn shared(prefix: &Path) -> Result<Self, PrefixError> {
        #[cfg(windows)]
        let directories = windows::directories(prefix)?;
        let file = lock_file(prefix)?;
        #[cfg(windows)]
        let identity = windows::identity(&file)?;
        for attempt in 0..START_ATTEMPTS {
            match file.try_lock_shared() {
                Ok(()) => {
                    #[cfg(not(windows))]
                    identified(&file, prefix)?;
                    #[cfg(windows)]
                    windows::revalidate(&identity, &prefix.join("lock"))?;
                    return Ok(Self {
                        _file: file,
                        #[cfg(windows)]
                        _directories: directories,
                        #[cfg(windows)]
                        _identity: identity,
                    });
                }
                Err(std::fs::TryLockError::WouldBlock) if attempt + 1 < START_ATTEMPTS => {
                    std::thread::sleep(START_INTERVAL);
                }
                Err(std::fs::TryLockError::WouldBlock) => return Err(PrefixError::StartBound),
                Err(std::fs::TryLockError::Error(error)) => return Err(io(error)),
            }
        }
        Err(PrefixError::StartBound)
    }
}

/// Recognize the ruled immutable layout, without guessing from a PID or a
/// service profile. An ordinary extracted package has no prefix guard.
pub fn prefix_of_binary(binary: &Path) -> Result<Option<PathBuf>, PrefixError> {
    let Some(bin) = binary
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "bin"))
    else {
        return Ok(None);
    };
    let Some(version) = bin.parent() else {
        return Ok(None);
    };
    let Some(versions) = version
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "versions"))
    else {
        return Ok(None);
    };
    let valid = version
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.len() == 40
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        });
    if !valid {
        return Err(PrefixError::Nonregular);
    }
    let prefix = versions.parent().ok_or(PrefixError::Nonregular)?;
    for path in [prefix, versions, version, bin] {
        directory(path)?;
    }
    Ok(Some(prefix.to_path_buf()))
}

#[cfg(test)]
mod tests;
