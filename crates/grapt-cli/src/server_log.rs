//! File-backed stderr custody. Rotation happens before launch, never while
//! the child is writing: an inherited file can exceed the threshold until
//! the next launch. No pipe or writer thread is involved.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

const ROTATE_BYTES: u64 = 5 * 1024 * 1024;

pub(crate) fn path(profile: &Path) -> PathBuf {
    profile.join("logs").join("server.log")
}

/// A refusal names the server log only when it is known to be a regular
/// file. Setup can fail before the first launch has created any log.
pub(crate) fn failure(profile: &Path, message: impl std::fmt::Display) -> String {
    if !std::fs::symlink_metadata(path(profile))
        .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
    {
        return message.to_string();
    }
    failure_for_platform(profile, message, cfg!(windows))
}

fn failure_for_platform(profile: &Path, message: impl std::fmt::Display, windows: bool) -> String {
    let log = path(profile);
    let display = if windows {
        crate::operator_text::windows_command_path(&log)
    } else {
        log.display().to_string()
    };
    format!("{message}; server log: {display}")
}

/// Serialize only preparation. The lock is released before this file is
/// handed to the child; it is never held across a server stderr write.
pub(crate) fn open(profile: &Path) -> io::Result<File> {
    let directory = profile.join("logs");
    match std::fs::create_dir(&directory) {
        Ok(()) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(&directory)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(io::Error::other(
                    "server log directory is not a real directory",
                ));
            }
        }
        Err(error) => return Err(error),
    }
    let lock_path = directory.join(".server-log.lock");
    regular_or_absent(&lock_path)?;
    // Windows locks require read or write access; FILE_APPEND_DATA alone
    // cannot acquire them. Only the log writer uses append-only access.
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(&lock_path)?;
    lock.try_lock()
        .map_err(|error| io::Error::other(format!("server log preparation lock: {error}")))?;
    let current = path(profile);
    let first = directory.join("server.log.1");
    let second = directory.join("server.log.2");
    let size = regular_or_absent(&current)?;
    if size.is_some_and(|size| size > ROTATE_BYTES) {
        // Retention is maintenance: a blocked archive or rename must not
        // prevent capturing stderr or launching an otherwise usable profile.
        let _rotation = rotate(&current, &first, &second);
    }
    let file = append(&current)?;
    lock.unlock()?;
    drop(lock);
    Ok(file)
}

fn regular_or_absent(path: &Path) -> io::Result<Option<u64>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            Ok(Some(metadata.len()))
        }
        Ok(_) => Err(io::Error::other("server log path is not a regular file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn rotate(current: &Path, first: &Path, second: &Path) -> io::Result<()> {
    regular_or_absent(first)?;
    regular_or_absent(second)?;
    remove_if_present(second)?;
    rename_if_present(first, second)?;
    std::fs::rename(current, first)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn rename_if_present(source: &Path, destination: &Path) -> io::Result<()> {
    match std::fs::rename(source, destination) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests;
