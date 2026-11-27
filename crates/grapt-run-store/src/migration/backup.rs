//! A verified copy of a run database, taken before a migration changes it.
//!
//! The copy is built beside the database's real file, so a symlinked path
//! still backs up onto the database's own filesystem. It is staged in a
//! directory created new for this backup (`0700` on Unix) and marked as the
//! store's, as a file created new (`0600` on Unix), whose identity is taken at
//! creation. Each step that names the copy by path first checks the path still
//! names that file: syncing, hard-linking it to a name no earlier backup holds
//! (a hard link fails rather than replace one), and removing the staging.
//! Nothing here writes to the live database.
//!
//! On Windows the staging and the backup take the directory's inherited ACL,
//! as any new file there does; an explicit ACL set on the database file itself
//! is not copied.
use crate::error::OpenError;
use crate::sqlite::sidecar;
use rusqlite::backup::{Backup, StepResult};
use rusqlite::Connection;
use same_file::Handle;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const STAGING: &str = ".backup-staging";
const COPY: &str = "copy";
/// Names this directory as the store's own staging, so cleanup never removes
/// files from a directory someone else made under the same name.
const MARKER: &str = ".grapt-staging";
const MARKER_BYTES: &[u8] = b"grapt run-store backup staging v1\n";
/// The copy and the sidecars SQLite may leave beside it.
const STAGED: [&str; 4] = [COPY, "copy-journal", "copy-wal", "copy-shm"];
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
    let staging = sidecar(path, STAGING);
    let result = clear(&staging)
        .and_then(|()| stage(&staging))
        .and_then(|()| Lease::create(&staging.join(COPY)))
        .map_err(OpenError::from)
        .and_then(|lease| {
            let published = copy(source, &lease.path)
                .and_then(|()| verify(&lease.path, from))
                .and_then(|()| lease.sync())
                .and_then(|()| publish(&lease, path, from));
            let released = lease.release();
            let name = published?;
            released?;
            clear(&staging)?;
            sync_directory(path)?;
            Ok(name)
        });
    if result.is_err() {
        // Best effort: the error that stopped the backup is the one reported.
        let _ = clear(&staging);
    }
    result.map_err(|error| match error {
        OpenError::DiskFull(_) | OpenError::StorageStalled(_) => error,
        other => OpenError::BackupFailed(Box::new(other)),
    })
}

/// Remove a staging directory left by a crash or by this backup. Every entry
/// is judged before anything is removed: the directory must hold only regular
/// files, each the marker, the copy or one of its SQLite sidecars, and any
/// copy needs the marker beside it with this store's bytes. An empty directory
/// is one a crash left before its marker. Anything else refuses and is left in
/// place.
fn clear(staging: &Path) -> io::Result<()> {
    match fs::symlink_metadata(staging) {
        Ok(meta) if meta.file_type().is_dir() => {}
        Ok(_) => return Err(not_ours(staging)),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    let (mut marked, mut staged) = (false, Vec::new());
    for entry in fs::read_dir(staging)? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file() {
            return Err(not_ours(&path));
        }
        let name = entry.file_name();
        if name == MARKER {
            marked = is_marker(&path)?;
            if !marked {
                return Err(not_ours(&path));
            }
        } else if STAGED.iter().any(|staged| name == *staged) {
            staged.push(path);
        } else {
            return Err(not_ours(&path));
        }
    }
    if !staged.is_empty() && !marked {
        return Err(not_ours(staging));
    }
    for file in staged {
        fs::remove_file(file)?;
    }
    if marked {
        fs::remove_file(staging.join(MARKER))?;
    }
    fs::remove_dir(staging)
}

fn is_marker(path: &Path) -> io::Result<bool> {
    let mut bytes = Vec::with_capacity(MARKER_BYTES.len() + 1);
    File::open(path)?
        .take(MARKER_BYTES.len() as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes == MARKER_BYTES)
}

fn not_ours(path: &Path) -> io::Error {
    io::Error::new(
        ErrorKind::AlreadyExists,
        format!(
            "{} is not this store's backup staging; move it away",
            path.display()
        ),
    )
}

/// Create the staging directory new, readable only by its owner on Unix, and
/// mark it as this store's before anything is staged in it.
fn stage(staging: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        DirBuilder::new().mode(0o700).create(staging)?;
    }
    #[cfg(not(unix))]
    {
        DirBuilder::new().create(staging)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut marker = options.open(staging.join(MARKER))?;
    marker.write_all(MARKER_BYTES)?;
    marker.sync_all()
}

/// The staged copy, held open from its creation until it is linked out.
struct Lease {
    path: PathBuf,
    file: File,
    identity: Handle,
}

impl Lease {
    /// Create the copy new, never opening an existing file, readable only by
    /// its owner on Unix. On Windows the handle refuses delete sharing, so the
    /// copy cannot be renamed or replaced while it is held. Its identity comes
    /// from a separate handle that allows deletion, confirmed against the
    /// created file, so closing the creating handle is enough to unlink it.
    fn create(path: &Path) -> io::Result<Self> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const SHARE_READ_WRITE: u32 = 3;
            options.share_mode(SHARE_READ_WRITE);
        }
        let file = options.open(path)?;
        let identity = Handle::from_path(path)?;
        if identity != Handle::from_file(file.try_clone()?)? {
            return Err(replaced(path));
        }
        Ok(Self {
            path: path.to_owned(),
            file,
            identity,
        })
    }

    fn ensure(&self, name: &Path) -> io::Result<()> {
        match Handle::from_path(name) {
            Ok(handle) if handle == self.identity => Ok(()),
            Ok(_) => Err(replaced(name)),
            Err(error) if error.kind() == ErrorKind::NotFound => Err(replaced(name)),
            Err(error) => Err(error),
        }
    }

    /// Sync the copy through the held handle once the path is known to still
    /// name it, so the bytes verified are the bytes made durable.
    fn sync(&self) -> Result<(), OpenError> {
        self.ensure(&self.path)?;
        Ok(self.file.sync_all()?)
    }

    /// Close both handles and remove the staged name if it is still this copy.
    /// A linked-out backup keeps the data under its own name.
    fn release(self) -> io::Result<()> {
        let ours = self.ensure(&self.path).is_ok();
        let Self {
            path,
            file,
            identity,
        } = self;
        drop(file);
        drop(identity);
        if ours {
            fs::remove_file(&path)?;
        }
        Ok(())
    }
}

fn replaced(name: &Path) -> io::Error {
    io::Error::new(
        ErrorKind::InvalidData,
        format!("{} was replaced during the backup", name.display()),
    )
}

fn copy(source: &Connection, staged: &Path) -> Result<(), OpenError> {
    let mut copy = Connection::open(staged)?;
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
fn verify(staged: &Path, from: u32) -> Result<(), OpenError> {
    let copy = Connection::open(staged)?;
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

/// Link the copy to the first free `<database>.v<from>[-n].backup` name. A
/// hard link fails when the name exists, so no earlier backup is replaced.
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
