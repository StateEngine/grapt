//! Bounded regular-file I/O for the local installer. Linux only in slice one.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

pub(super) fn regular(path: &Path) -> Result<File, String> {
    directory(path.parent().ok_or("installation input has no parent")?)?;
    let before = std::fs::symlink_metadata(path)
        .map_err(|error| format!("installation input cannot be inspected: {:?}", error.kind()))?;
    if !before.is_file() || before.file_type().is_symlink() {
        return Err("installation input must be a regular file, not a link".into());
    }
    directory(path.parent().ok_or("installation input has no parent")?)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("installation input cannot be opened: {:?}", error.kind()))?;
    let opened = file.metadata().map_err(|error| {
        format!(
            "installation input cannot be identified: {:?}",
            error.kind()
        )
    })?;
    if !opened.is_file() || before.dev() != opened.dev() || before.ino() != opened.ino() {
        return Err("installation input changed while it was opened".into());
    }
    Ok(file)
}

pub(super) fn read(path: &Path, cap: u64) -> Result<Vec<u8>, String> {
    let file = regular(path)?;
    let mut bytes = Vec::new();
    file.take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("installation input cannot be read: {:?}", error.kind()))?;
    if bytes.len() as u64 > cap {
        return Err("installation input exceeds its byte bound".into());
    }
    Ok(bytes)
}

pub(super) fn directory(path: &Path) -> Result<(), String> {
    if path.components().count() > 64 {
        return Err("installation directory exceeds its component bound".into());
    }
    let mut current = std::path::PathBuf::new();
    for component in path.components() {
        current.push(component);
        let metadata = std::fs::symlink_metadata(&current).map_err(|error| {
            format!(
                "installation directory cannot be inspected: {:?}",
                error.kind()
            )
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("installation directory must be a real directory, not a link".into());
        }
    }
    // The per-user prefix is trusted against same-user replacement. These
    // immediate checks detect links/corruption; they do not promise openat-style
    // isolation from a process that can already replace the user's binaries.
    if current.as_os_str().is_empty() {
        return Err("installation directory has no path".into());
    }
    Ok(())
}

pub(super) fn sync_directory(path: &Path) -> Result<(), String> {
    directory(path)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| {
            format!(
                "installation directory cannot be opened: {:?}",
                error.kind()
            )
        })?;
    file.sync_all().map_err(|error| {
        format!(
            "installation directory cannot be synced: {:?}",
            error.kind()
        )
    })
}

pub(super) fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    directory(path.parent().ok_or("installation record has no parent")?)?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("installation record cannot be created: {:?}", error.kind()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("installation record cannot be synced: {:?}", error.kind()))
}

pub(super) fn replace(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("installation record has no parent")?;
    directory(parent)?;
    let name = path.file_name().ok_or("installation record has no name")?;
    let mut temporary = name.to_os_string();
    temporary.push(".new");
    let temporary = parent.join(temporary);
    // Only the exclusive prefix owner may remove its interrupted temporary.
    match std::fs::symlink_metadata(&temporary) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            std::fs::remove_file(&temporary).map_err(|error| {
                format!(
                    "installation temporary cannot be removed: {:?}",
                    error.kind()
                )
            })?;
        }
        Ok(_) => return Err("installation temporary is not a regular file".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "installation temporary is unreadable: {:?}",
                error.kind()
            ))
        }
    }
    write_new(&temporary, bytes)?;
    std::fs::rename(&temporary, path)
        .map_err(|error| format!("installation record cannot be replaced: {:?}", error.kind()))?;
    sync_directory(parent)
}
