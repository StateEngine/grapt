use grapt_types::DatabaseFingerprint;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use std::io::Read as _;
use std::path::{Path, PathBuf};

#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt as _;

const HASH_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("snapshot source {0:?} is absent")]
    SourceAbsent(PathBuf),
    #[error("snapshot destination {0:?} already exists")]
    DestinationExists(PathBuf),
    #[error("snapshot io: {0}")]
    Io(#[from] std::io::Error),
    #[error("snapshot sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("snapshot integrity check returned {0:?}")]
    Integrity(String),
    #[error("database changed while its read-only snapshot was taken")]
    SourceChanged,
    #[error("database fingerprint construction refused")]
    InvalidFingerprint,
    #[error("database byte count overflowed u64")]
    ByteCountOverflow,
    #[error("physical identity for {path:?} is unreadable: {source}")]
    PhysicalIdentityUnreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("database artifacts {first:?} and {second:?} name the same physical file")]
    PhysicalAlias { first: PathBuf, second: PathBuf },
    #[error("database artifact {0:?} changed physical identity before a write boundary")]
    PhysicalIdentityChanged(PathBuf),
}

/// A retained supported-platform identity, not a lexical path identity.
///
/// `same_file::Handle` compares device/inode on Unix and volume/file-index on
/// Windows. The retained open handle lets write boundaries detect path swaps.
#[derive(Debug)]
pub(crate) struct PhysicalFileIdentity {
    path: PathBuf,
    handle: same_file::Handle,
}

impl PhysicalFileIdentity {
    pub(crate) fn required(path: &Path) -> Result<Self, SnapshotError> {
        match Self::optional(path)? {
            Some(identity) => Ok(identity),
            None => Err(SnapshotError::SourceAbsent(path.to_path_buf())),
        }
    }

    pub(crate) fn optional(path: &Path) -> Result<Option<Self>, SnapshotError> {
        let file = match open_identity_file(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::symlink_metadata(path) {
                    Err(metadata_error)
                        if metadata_error.kind() == std::io::ErrorKind::NotFound =>
                    {
                        return Ok(None);
                    }
                    Ok(_) => {
                        return Err(SnapshotError::PhysicalIdentityUnreadable {
                            path: path.to_path_buf(),
                            source: error,
                        });
                    }
                    Err(source) => {
                        return Err(SnapshotError::PhysicalIdentityUnreadable {
                            path: path.to_path_buf(),
                            source,
                        });
                    }
                }
            }
            Err(source) => {
                return Err(SnapshotError::PhysicalIdentityUnreadable {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let handle = same_file::Handle::from_file(file).map_err(|source| {
            SnapshotError::PhysicalIdentityUnreadable {
                path: path.to_path_buf(),
                source,
            }
        })?;
        let metadata = handle.as_file().metadata().map_err(|source| {
            SnapshotError::PhysicalIdentityUnreadable {
                path: path.to_path_buf(),
                source,
            }
        })?;
        if !metadata.is_file() {
            return Err(SnapshotError::PhysicalIdentityUnreadable {
                path: path.to_path_buf(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "database artifact is not a regular file",
                ),
            });
        }
        Ok(Some(Self {
            path: path.to_path_buf(),
            handle,
        }))
    }

    pub(crate) fn revalidate(&self) -> Result<(), SnapshotError> {
        let observed = same_file::Handle::from_path(&self.path).map_err(|source| {
            SnapshotError::PhysicalIdentityUnreadable {
                path: self.path.clone(),
                source,
            }
        })?;
        if observed == self.handle {
            Ok(())
        } else {
            Err(SnapshotError::PhysicalIdentityChanged(self.path.clone()))
        }
    }

    pub(crate) fn try_clone_file(&self) -> Result<std::fs::File, SnapshotError> {
        self.handle
            .as_file()
            .try_clone()
            .map_err(SnapshotError::from)
    }

    fn same_file_as(&self, other: &Self) -> bool {
        self.handle == other.handle
    }
}

fn open_identity_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        // Keep replacement/rename impossible while this identity lease lives.
        // SQLite still receives read/write sharing, but FILE_SHARE_DELETE is
        // deliberately absent. The Unix authority opens SQLite through the
        // retained descriptor instead (see grapt-store).
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
    }
    options.open(path)
}

pub(crate) fn require_distinct_physical_files(
    identities: &[&PhysicalFileIdentity],
) -> Result<(), SnapshotError> {
    for (index, first) in identities.iter().enumerate() {
        for second in &identities[index + 1..] {
            if first.same_file_as(second) {
                return Err(SnapshotError::PhysicalAlias {
                    first: first.path.clone(),
                    second: second.path.clone(),
                });
            }
        }
    }
    Ok(())
}

// Retained handles use device/inode on Unix and volume/file-index on Windows,
// so this boundary refuses hard links and reparse-point aliases alike.
pub(crate) fn revalidate_distinct_physical_files(
    identities: &[&PhysicalFileIdentity],
) -> Result<(), SnapshotError> {
    for identity in identities {
        identity.revalidate()?;
    }
    require_distinct_physical_files(identities)
}

pub fn fingerprint_file(path: &Path) -> Result<(u64, DatabaseFingerprint), SnapshotError> {
    let mut file = std::fs::File::open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            SnapshotError::SourceAbsent(path.to_path_buf())
        } else {
            SnapshotError::Io(error)
        }
    })?;
    if !file.metadata()?.is_file() {
        return Err(SnapshotError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "database path is not a file",
        )));
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];
    let mut bytes = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let read_u64 = u64::try_from(read).map_err(|_| SnapshotError::ByteCountOverflow)?;
        bytes = bytes
            .checked_add(read_u64)
            .ok_or(SnapshotError::ByteCountOverflow)?;
        hasher.update(&buffer[..read]);
    }
    let digest = format!("{:x}", hasher.finalize());
    let fingerprint = DatabaseFingerprint::new(digest).ok_or(SnapshotError::InvalidFingerprint)?;
    Ok((bytes, fingerprint))
}

pub(crate) fn open_read_only(path: &Path) -> Result<Connection, SnapshotError> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(SnapshotError::from)
}

pub(crate) fn create_verified_snapshot(
    source: &Path,
    destination: &Path,
) -> Result<(u64, DatabaseFingerprint), SnapshotError> {
    if destination.exists() {
        return Err(SnapshotError::DestinationExists(destination.to_path_buf()));
    }
    let pending = pending_path(destination);
    if pending.exists() {
        std::fs::remove_file(&pending)?;
    }
    let (_, before) = fingerprint_file(source)?;
    let source_connection = open_read_only(source)?;
    let created = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)?;
    created.sync_all()?;
    drop(created);
    let mut destination_connection = Connection::open_with_flags(
        &pending,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI,
    )?;
    {
        let backup =
            rusqlite::backup::Backup::new(&source_connection, &mut destination_connection)?;
        backup.run_to_completion(64, std::time::Duration::from_millis(5), None)?;
    }
    let verdict: String =
        destination_connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if verdict != "ok" {
        return Err(SnapshotError::Integrity(verdict));
    }
    drop(destination_connection);
    drop(source_connection);
    let (_, after) = fingerprint_file(source)?;
    if before != after {
        return Err(SnapshotError::SourceChanged);
    }
    let result = fingerprint_file(&pending)?;
    std::fs::hard_link(&pending, destination).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            SnapshotError::DestinationExists(destination.to_path_buf())
        } else {
            SnapshotError::Io(error)
        }
    })?;
    std::fs::remove_file(pending)?;
    Ok(result)
}

fn pending_path(destination: &Path) -> PathBuf {
    let mut value = destination.as_os_str().to_owned();
    value.push(".pending");
    PathBuf::from(value)
}
