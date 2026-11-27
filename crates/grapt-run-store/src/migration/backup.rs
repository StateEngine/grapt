//! A verified copy of a run database, taken before a migration changes it.
//!
//! The copy is written beside the database's real file, so a symlinked path
//! still backs up onto the database's own filesystem. It is built in a fixed
//! partial file that the writer lock makes ours alone, created new with the
//! database's permissions, checked on a fresh connection and synced. It is
//! then hard-linked to a name no earlier backup holds, which fails rather
//! than replace anything. Nothing here writes to the live database.
use crate::error::OpenError;
use crate::sqlite::sidecar;
use rusqlite::backup::{Backup, StepResult};
use rusqlite::Connection;
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::Duration;

const PARTIAL: &str = ".backup-partial";
const PAGES_PER_STEP: i32 = 1024;
const MAX_NAMES: u32 = 1024;
/// A source lock held past this many pauses stalls the backup, about two
/// seconds in all.
const MAX_STALLS: u32 = 100;
const STALL_PAUSE: Duration = Duration::from_millis(20);

/// Copy the database at `path`, at schema version `from`, from `source`, and
/// return where the verified copy now lives. Storage pressure is reported as
/// itself; any other failure as [`OpenError::BackupFailed`].
pub(super) fn take(source: &Connection, path: &Path, from: u32) -> Result<PathBuf, OpenError> {
    let partial = sidecar(path, PARTIAL);
    let result = discard(&partial)
        .and_then(|()| create(&partial, path))
        .map_err(OpenError::from)
        .and_then(|()| copy(source, &partial))
        .and_then(|()| verify(&partial, from))
        .and_then(|()| publish(&partial, path, from));
    if result.is_err() {
        // Best effort: the error that stopped the backup is the one reported.
        let _ = discard(&partial);
    }
    result.map_err(|error| match error {
        OpenError::DiskFull(_) | OpenError::StorageStalled(_) => error,
        other => OpenError::BackupFailed(Box::new(other)),
    })
}

/// Remove a partial copy and its SQLite sidecars, left by a crash or by this
/// attempt. Only regular files are removed; a link, directory or anything
/// else at one of these names refuses and is left in place.
fn discard(partial: &Path) -> io::Result<()> {
    for file in [
        partial.to_owned(),
        sidecar(partial, "-journal"),
        sidecar(partial, "-wal"),
        sidecar(partial, "-shm"),
    ] {
        match fs::symlink_metadata(&file) {
            Ok(meta) if meta.file_type().is_file() => fs::remove_file(&file)?,
            Ok(_) => {
                return Err(io::Error::new(
                    ErrorKind::AlreadyExists,
                    format!("{} is not a regular file; move it away", file.display()),
                ))
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Create the partial file new, never opening an existing one, with the
/// database's permission bits so the copy is no more readable than its source.
fn create(partial: &Path, path: &Path) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(fs::metadata(path)?.permissions().mode() & 0o777);
    }
    #[cfg(not(unix))]
    let _ = path;
    options.open(partial).map(drop)
}

fn copy(source: &Connection, partial: &Path) -> Result<(), OpenError> {
    let mut copy = Connection::open(partial)?;
    {
        let backup = Backup::new(source, &mut copy)?;
        let mut stalls = 0;
        loop {
            match backup.step(PAGES_PER_STEP)? {
                StepResult::Done => break,
                StepResult::More => {}
                _ if stalls < MAX_STALLS => {
                    stalls += 1;
                    std::thread::sleep(STALL_PAUSE);
                }
                _ => {
                    return Err(OpenError::StorageStalled(Box::new(io::Error::new(
                        ErrorKind::TimedOut,
                        "the database stayed locked while it was being backed up",
                    ))))
                }
            }
        }
    }
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

/// Link the copy to the first free `<database>.v<from>[-n].backup` name. A
/// hard link fails when the name exists, so no earlier backup is replaced.
fn publish(partial: &Path, path: &Path, from: u32) -> Result<PathBuf, OpenError> {
    for attempt in 0..MAX_NAMES {
        let suffix = match attempt {
            0 => format!(".v{from}.backup"),
            n => format!(".v{from}-{n}.backup"),
        };
        let name = sidecar(path, &suffix);
        match fs::hard_link(partial, &name) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
        fs::remove_file(partial)?;
        sync_directory(path)?;
        return Ok(name);
    }
    Err(io::Error::new(
        ErrorKind::AlreadyExists,
        format!("{MAX_NAMES} backups of version {from} already exist beside the database"),
    )
    .into())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(directory) => fs::File::open(directory)?.sync_all(),
        None => Ok(()),
    }
}

/// Windows offers no directory handle to sync; NTFS journals the link, and
/// the file's own data was synced before it was linked.
#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}
