//! Windows installer I/O preparation; not connected to command dispatch yet.

use std::fs::{File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf, Prefix};

const REPARSE: u32 = 0x0000_0400;
const OPEN_REPARSE: u32 = 0x0020_0000;
const BACKUP_SEMANTICS: u32 = 0x0200_0000;
const SHARE_READ_WRITE: u32 = 3;
const SHARE_ALL: u32 = 7;

fn failure(context: &str, error: std::io::Error) -> String {
    format!("installation {context} refused: {:?}", error.kind())
}

fn linked(metadata: &Metadata) -> bool {
    metadata.file_type().is_symlink() || metadata.file_attributes() & REPARSE != 0
}

pub(super) fn direct_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute() || path.components().count() > 64 {
        return Err("installation directory requires a bounded absolute disk path".into());
    }
    for component in path.components() {
        match component {
            Component::Prefix(value)
                if matches!(value.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) => {}
            Component::RootDir | Component::Normal(_) => {}
            _ => return Err("installation directory is not a direct disk route".into()),
        }
    }
    Ok(())
}

pub(super) fn directory(path: &Path) -> Result<(), String> {
    direct_path(path)?;
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(value)
                if matches!(value.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) =>
            {
                current.push(component);
                continue;
            }
            Component::RootDir | Component::Normal(_) => current.push(component),
            _ => return Err("installation directory is not a direct disk route".into()),
        }
        let metadata = std::fs::symlink_metadata(&current)
            .map_err(|error| failure("directory inspection", error))?;
        if !metadata.is_dir() || linked(&metadata) {
            return Err("installation directory must be real, not a reparse point".into());
        }
    }
    Ok(())
}

pub(super) fn regular(path: &Path) -> Result<File, String> {
    regular_with_sharing(path, SHARE_READ_WRITE)
}

// Temporary hashing readers close before inactive staging is moved. Final-name
// snapshots retain metadata identities only after that move.
pub(super) fn snapshot_regular(path: &Path) -> Result<File, String> {
    regular_with_sharing(path, SHARE_READ_WRITE)
}

/// Keep the exact bytes immutable while a pathname-based signature reader runs.
pub(super) fn immutable_regular(path: &Path) -> Result<File, String> {
    regular_with_sharing(path, 1)
}

fn regular_with_sharing(path: &Path, sharing: u32) -> Result<File, String> {
    directory(path.parent().ok_or("installation input has no parent")?)?;
    let before =
        std::fs::symlink_metadata(path).map_err(|error| failure("input inspection", error))?;
    if !before.is_file() || linked(&before) {
        return Err("installation input must be a regular file, not a reparse point".into());
    }
    let named =
        same_file::Handle::from_path(path).map_err(|error| failure("input identity", error))?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(OPEN_REPARSE)
        .share_mode(sharing)
        .open(path)
        .map_err(|error| failure("input open", error))?;
    let opened = file
        .metadata()
        .map_err(|error| failure("input metadata", error))?;
    let actual = same_file::Handle::from_file(
        file.try_clone()
            .map_err(|error| failure("input handle", error))?,
    )
    .map_err(|error| failure("input identity", error))?;
    let after = std::fs::symlink_metadata(path).map_err(|error| failure("input recheck", error))?;
    if !opened.is_file() || linked(&opened) || linked(&after) || named != actual {
        return Err("installation input changed while it was opened".into());
    }
    Ok(file)
}

// Retain physical identity at the final name without retaining data access.
// Windows still refuses moving an ancestor until these handles are closed.
pub(super) fn snapshot_identity(path: &Path, is_directory: bool) -> Result<File, String> {
    if is_directory {
        directory(path)?;
    } else {
        directory(path.parent().ok_or("installation input has no parent")?)?;
    }
    let flags = OPEN_REPARSE | if is_directory { BACKUP_SEMANTICS } else { 0 };
    let file = OpenOptions::new()
        .read(true)
        .access_mode(0)
        .custom_flags(flags)
        .share_mode(SHARE_ALL)
        .open(path)
        .map_err(|error| failure("snapshot identity open", error))?;
    let metadata = file
        .metadata()
        .map_err(|error| failure("snapshot identity metadata", error))?;
    if linked(&metadata)
        || metadata.is_dir() != is_directory
        || (!is_directory && !metadata.is_file())
    {
        return Err("installation snapshot input changed while opening".into());
    }
    Ok(file)
}

pub(super) fn read(path: &Path, cap: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    regular(path)?
        .take(
            cap.checked_add(1)
                .ok_or("installation read bound overflows")?,
        )
        .read_to_end(&mut bytes)
        .map_err(|error| failure("input read", error))?;
    if bytes.len() as u64 > cap {
        return Err("installation input exceeds its byte bound".into());
    }
    Ok(bytes)
}

pub(super) fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    directory(path.parent().ok_or("installation record has no parent")?)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(OPEN_REPARSE)
        .share_mode(SHARE_READ_WRITE)
        .open(path)
        .map_err(|error| failure("record create", error))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| failure("record sync", error))
}

pub(super) fn sync_directory(path: &Path) -> Result<(), String> {
    directory(path)?;
    // FlushFileBuffers requires GENERIC_WRITE. A read-only backup-semantics
    // handle cannot establish durability. Native qualification must prove the
    // write-capable directory flush; a refusal is propagated, never waived.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(BACKUP_SEMANTICS | OPEN_REPARSE)
        .share_mode(SHARE_ALL)
        .open(path)
        .map_err(|error| failure("directory flush open", error))?;
    let metadata = file
        .metadata()
        .map_err(|error| failure("directory flush metadata", error))?;
    if !metadata.is_dir() || linked(&metadata) {
        return Err("installation directory changed before its flush".into());
    }
    file.sync_all()
        .map_err(|error| failure("directory sync", error))
}

pub(super) fn replace(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("installation record has no parent")?;
    directory(parent)?;
    let mut name = path
        .file_name()
        .ok_or("installation record has no name")?
        .to_os_string();
    name.push(".new");
    let temporary = parent.join(name);
    match std::fs::symlink_metadata(&temporary) {
        Ok(metadata) if metadata.is_file() && !linked(&metadata) => {
            // Acquire physical identity and a no-delete lease before inspecting
            // the interrupted record. Release our lease before its exact unlink.
            drop(regular(&temporary)?);
            std::fs::remove_file(&temporary)
                .map_err(|error| failure("temporary removal", error))?;
        }
        Ok(_) => return Err("installation temporary must be a regular file".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(failure("temporary inspection", error)),
    }
    write_new(&temporary, bytes)?;
    // The existing transition owner holds prefix EX. std::fs::rename uses the
    // admitted same-volume replace; a held target reports its sharing refusal.
    std::fs::rename(&temporary, path).map_err(|error| failure("record replace", error))?;
    sync_directory(parent)
}

#[cfg(test)]
#[path = "windows_io_tests.rs"]
mod tests;
