//! A verified copy of a run database, taken before a migration changes it.
//!
//! The copy is written beside the database's real file, so a symlinked path
//! still backs up onto the database's own filesystem and the final rename is
//! atomic. It is built under a fixed partial name that the writer lock makes
//! ours alone, checked on a fresh connection, synced, and only then renamed to
//! a name no earlier backup holds. Nothing here touches the live database.
use crate::error::OpenError;
use crate::sqlite::sidecar;
use rusqlite::backup::Backup;
use rusqlite::Connection;
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

const PARTIAL: &str = ".backup-partial";
const PAGES_PER_STEP: i32 = 1024;
const MAX_NAMES: u32 = 1024;

/// Copy the database at `path`, at schema version `from`, and return where the
/// verified copy now lives.
pub(super) fn take(source: &Connection, path: &Path, from: u32) -> Result<PathBuf, OpenError> {
    let partial = sidecar(path, PARTIAL);
    let result = discard(&partial)
        .map_err(OpenError::from)
        .and_then(|()| copy(source, &partial))
        .and_then(|()| verify(&partial, from))
        .and_then(|()| publish(&partial, path, from));
    if result.is_err() {
        // Best effort: the error that stopped the backup is the one reported.
        let _ = discard(&partial);
    }
    result.map_err(|error| OpenError::BackupFailed(Box::new(error)))
}

/// Remove a partial copy and its SQLite sidecars, left by a crash or by this
/// attempt. Only regular files are removed; anything else refuses.
fn discard(partial: &Path) -> std::io::Result<()> {
    for file in [
        partial.to_owned(),
        sidecar(partial, "-journal"),
        sidecar(partial, "-wal"),
        sidecar(partial, "-shm"),
    ] {
        match fs::remove_file(&file) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn copy(source: &Connection, partial: &Path) -> Result<(), OpenError> {
    let mut copy = Connection::open(partial)?;
    Backup::new(source, &mut copy)?.run_to_completion(PAGES_PER_STEP, Duration::ZERO, None)?;
    copy.close().map_err(|(_, error)| error)?;
    Ok(())
}

/// Reopen the copy from disk, make it one self-contained file, and check it is
/// intact and at the version it was taken from.
fn verify(partial: &Path, from: u32) -> Result<(), OpenError> {
    let copy = Connection::open(partial)?;
    let mode: String = copy.query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))?;
    if mode != "delete" {
        return Err(OpenError::BackupUnverified(format!("journal mode {mode}")));
    }
    let check: String = copy.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if check != "ok" {
        return Err(OpenError::BackupUnverified(check));
    }
    let version: i64 = copy.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != i64::from(from) {
        return Err(OpenError::BackupUnverified(format!(
            "schema version {version}, expected {from}"
        )));
    }
    copy.close().map_err(|(_, error)| error)?;
    OpenOptions::new().write(true).open(partial)?.sync_all()?;
    Ok(())
}

/// Rename the copy to the first free `<database>.v<from>[-n].backup` name. An
/// earlier backup is never replaced; the writer lock keeps the name free
/// between the check and the rename.
fn publish(partial: &Path, path: &Path, from: u32) -> Result<PathBuf, OpenError> {
    for attempt in 0..MAX_NAMES {
        let suffix = match attempt {
            0 => format!(".v{from}.backup"),
            n => format!(".v{from}-{n}.backup"),
        };
        let name = sidecar(path, &suffix);
        match fs::symlink_metadata(&name) {
            Ok(_) => continue,
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        fs::rename(partial, &name)?;
        sync_directory(path)?;
        return Ok(name);
    }
    Err(std::io::Error::new(
        ErrorKind::AlreadyExists,
        format!("{MAX_NAMES} backups of version {from} already exist beside the database"),
    )
    .into())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    match path.parent() {
        Some(directory) => fs::File::open(directory)?.sync_all(),
        None => Ok(()),
    }
}

/// Windows offers no directory handle to sync; NTFS journals the rename.
#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
