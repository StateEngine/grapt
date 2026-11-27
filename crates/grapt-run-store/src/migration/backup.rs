//! A verified copy of a run database, taken before a migration changes it.
//!
//! The copy is written beside the database's real file, so a symlinked path
//! still backs up onto the database's own filesystem. It is built in a fixed
//! partial file, created new and held open for the whole backup so its
//! identity is known. Every step that names the partial by path first checks
//! that the path still names that file: verifying, syncing, linking it to a
//! name no earlier backup holds (a hard link fails rather than replace one),
//! and removing it. Nothing here writes to the live database.
use crate::error::OpenError;
use crate::sqlite::sidecar;
use rusqlite::backup::{Backup, StepResult};
use rusqlite::Connection;
use same_file::Handle;
use std::fs::{self, File, OpenOptions};
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
        .and_then(|()| Lease::create(&partial, path))
        .map_err(OpenError::from)
        .and_then(|lease| {
            let published = copy(source, &partial)
                .and_then(|()| verify(&partial, from))
                .and_then(|()| lease.sync())
                .and_then(|()| publish(&lease, path, from));
            match published {
                Ok(name) => lease.release().map(|()| name).map_err(OpenError::from),
                Err(error) => {
                    // Best effort: the error that stopped the backup is reported.
                    let _ = lease.abandon();
                    Err(error)
                }
            }
        });
    result.map_err(|error| match error {
        OpenError::DiskFull(_) | OpenError::StorageStalled(_) => error,
        other => OpenError::BackupFailed(Box::new(other)),
    })
}

/// Remove a partial copy left by a crash, with its SQLite sidecars. Only
/// regular files are removed; a link, directory or anything else at one of
/// these names refuses and is left in place.
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

/// The partial file this backup created, held open from creation to removal.
struct Lease {
    path: PathBuf,
    file: File,
    identity: Handle,
}

impl Lease {
    /// Create the partial new, never opening an existing file. On Unix it gets
    /// the database's permission bits, so the copy is no more readable than
    /// its source. On Windows it takes the directory's inherited ACL, as any
    /// new file there does; an explicit ACL set on the database file itself is
    /// not copied. The handle refuses delete sharing there, so the partial
    /// cannot be renamed or replaced while it is held.
    fn create(partial: &Path, path: &Path) -> io::Result<Self> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            options.mode(fs::metadata(path)?.permissions().mode() & 0o777);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const SHARE_READ_WRITE: u32 = 3;
            options.share_mode(SHARE_READ_WRITE);
            let _ = path;
        }
        let file = options.open(partial)?;
        let identity = Handle::from_file(file.try_clone()?)?;
        Ok(Self {
            path: partial.to_owned(),
            file,
            identity,
        })
    }

    fn ensure(&self, name: &Path) -> io::Result<()> {
        if names(&self.identity, name)? {
            return Ok(());
        }
        Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("{} was replaced during the backup", name.display()),
        ))
    }

    /// Sync the copy through the held handle once the path is known to still
    /// name it, so the bytes verified are the bytes made durable.
    fn sync(&self) -> Result<(), OpenError> {
        self.ensure(&self.path)?;
        Ok(self.file.sync_all()?)
    }

    /// Release the partial name once the copy is linked elsewhere. The handle
    /// closes first, since Windows refuses to unlink a file held without
    /// delete sharing, and the name is removed only if it is still ours.
    fn release(self) -> io::Result<()> {
        drop(self.file);
        if names(&self.identity, &self.path)? {
            fs::remove_file(&self.path)?;
        }
        Ok(())
    }

    /// Remove what this backup created after a failure, and nothing else.
    fn abandon(self) -> io::Result<()> {
        drop(self.file);
        if names(&self.identity, &self.path)? {
            discard(&self.path)?;
        }
        Ok(())
    }
}

/// Whether `name` is the file `identity` holds; a missing name is not.
fn names(identity: &Handle, name: &Path) -> io::Result<bool> {
    match Handle::from_path(name) {
        Ok(handle) => Ok(handle == *identity),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
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
    Ok(())
}

/// Link the copy to the first free `<database>.v<from>[-n].backup` name, then
/// release the partial name. A hard link fails when the name exists, so no
/// earlier backup is replaced.
fn publish(lease: &Lease, path: &Path, from: u32) -> Result<PathBuf, OpenError> {
    for attempt in 0..MAX_NAMES {
        let suffix = match attempt {
            0 => format!(".v{from}.backup"),
            n => format!(".v{from}-{n}.backup"),
        };
        let name = sidecar(path, &suffix);
        lease.ensure(&lease.path)?;
        match fs::hard_link(&lease.path, &name) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
        lease.ensure(&name)?;
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
        Some(directory) => File::open(directory)?.sync_all(),
        None => Ok(()),
    }
}

/// FlushFileBuffers needs a write-capable directory handle; a refusal is
/// reported, never waived.
#[cfg(windows)]
fn sync_directory(path: &Path) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const SHARE_ALL: u32 = 7;
    match path.parent() {
        Some(directory) => OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(BACKUP_SEMANTICS)
            .share_mode(SHARE_ALL)
            .open(directory)?
            .sync_all(),
        None => Ok(()),
    }
}
