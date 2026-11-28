use super::preflight::{FutureBackup, MigrationPlan, StoreReadiness, Unreadable};
use super::{Migration, MigrationError, MigrationOutcome};
use grapt_types::{ClockAuthority, MigrationId};
use std::fs::File;
use std::path::{Path, PathBuf};

#[cfg(all(unix, not(target_os = "macos")))]
use std::os::fd::AsRawFd as _;

#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt as _;

#[cfg(target_os = "macos")]
mod macos_binding;

/// Physical identity carried across owned-open's writable boundaries.
///
/// `same_file::Handle` retains the opened OS handle. Its equality key is the
/// device/inode pair on Unix and the volume serial/file index pair on Windows.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DatabaseFileIdentity(same_file::Handle);

#[derive(Debug)]
pub(crate) struct DatabaseFileHandle {
    identity: DatabaseFileIdentity,
}

impl DatabaseFileHandle {
    pub(crate) fn identity(&self) -> &DatabaseFileIdentity {
        &self.identity
    }

    pub(crate) fn from_file(file: File) -> Result<Self, std::io::Error> {
        let identity = DatabaseFileIdentity(same_file::Handle::from_file(file)?);
        if !identity.0.as_file().metadata()?.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "database path is not a file",
            ));
        }
        Ok(Self { identity })
    }

    pub(crate) fn revalidate(&self) -> Result<(), std::io::Error> {
        let observed = DatabaseFileIdentity(same_file::Handle::from_file(
            self.identity.0.as_file().try_clone()?,
        )?);
        if observed == self.identity {
            Ok(())
        } else {
            Err(std::io::Error::other(
                "opened database handle changed identity",
            ))
        }
    }

    pub(crate) fn open_sqlite(
        &self,
        _display_path: &Path,
        flags: rusqlite::OpenFlags,
    ) -> Result<rusqlite::Connection, rusqlite::Error> {
        // SQLite's Unix VFS resolves Linux /proc/self/fd symlinks to the real
        // filename, so rollback and WAL sidecars are created beside that file.
        // Darwin /dev/fd duplicates the retained descriptor instead; it cannot
        // upgrade a read-only handle and is not a directory for sidecars.
        #[cfg(all(unix, not(target_os = "macos")))]
        let target = {
            let descriptor = self.identity.0.as_file().as_raw_fd();
            #[cfg(any(target_os = "linux", target_os = "android"))]
            let root = "/proc/self/fd";
            #[cfg(not(any(target_os = "linux", target_os = "android")))]
            let root = "/dev/fd";
            PathBuf::from(format!("{root}/{descriptor}"))
        };
        #[cfg(windows)]
        let target = _display_path.to_path_buf();
        #[cfg(target_os = "macos")]
        let binding = macos_binding::SqliteBinding::new(self.identity())
            .map_err(macos_binding::sqlite_error)?;
        #[cfg(target_os = "macos")]
        let target = binding.target();
        #[cfg(all(test, target_os = "macos"))]
        macos_binding_tests::before_open().map_err(macos_binding::sqlite_error)?;
        let opened = rusqlite::Connection::open_with_flags(
            target,
            flags | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        );
        #[cfg(all(test, target_os = "macos"))]
        macos_binding_tests::after_open().map_err(macos_binding::sqlite_error)?;
        let conn = opened?;
        #[cfg(target_os = "macos")]
        binding
            .validate(self.identity())
            .map_err(macos_binding::sqlite_error)?;
        Ok(conn)
    }
}

/// The file a database path names, with a final file link resolved. SQLite
/// reads and writes, its `-wal` and `-journal` sidecars and the backup must
/// all use the real file: SQLite on Windows would otherwise put sidecars
/// beside the link, where an open through the real path never finds them.
pub(crate) fn real_database_path(path: &Path) -> Result<PathBuf, std::io::Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => std::fs::canonicalize(path),
        _ => Ok(path.to_path_buf()),
    }
}

pub(crate) fn database_file_handle(path: &Path) -> Result<DatabaseFileHandle, std::io::Error> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
    }
    DatabaseFileHandle::from_file(options.open(path)?)
}

pub(crate) fn database_file_identity(path: &Path) -> Result<DatabaseFileIdentity, std::io::Error> {
    database_file_handle(path).map(|handle| handle.identity)
}

/// One writable SQLite authority bound to a retained physical file.
///
/// Linux opens through the retained descriptor's procfs symlink. macOS derives
/// the retained vnode's path and rejects any vnode/ancestor replacement across
/// SQLite open, including a substitution restored before the identity check;
/// Windows also uses the path while the retained handle denies delete/rename
/// sharing. Every path must still name the retained physical file at the owned
/// open boundaries.
#[derive(Debug)]
pub struct BoundDatabaseFile {
    path: PathBuf,
    handle: DatabaseFileHandle,
}

#[derive(Debug, thiserror::Error)]
pub enum BoundDatabaseError {
    #[error("database file identity could not be established: {0}")]
    Identity(#[source] std::io::Error),
    #[error("database path no longer names the bound physical file")]
    PathIdentityChanged,
    #[error(transparent)]
    Migration(#[from] MigrationError),
    #[error(transparent)]
    Open(#[from] crate::WriteError),
}

impl BoundDatabaseFile {
    pub fn from_file(path: &Path, file: File) -> Result<Self, BoundDatabaseError> {
        let handle = DatabaseFileHandle::from_file(file).map_err(BoundDatabaseError::Identity)?;
        validate_bound_path(path, &handle).map_err(|error| match error {
            MigrationError::PathIdentityChanged => BoundDatabaseError::PathIdentityChanged,
            MigrationError::FileIdentity(source) => BoundDatabaseError::Identity(source),
            other => BoundDatabaseError::Migration(other),
        })?;
        Ok(Self {
            path: path.to_path_buf(),
            handle,
        })
    }

    pub fn migrate_with_clock(
        &self,
        backup_dir: &Path,
        engine_version: &str,
        clock: &dyn ClockAuthority,
    ) -> Result<MigrationOutcome, BoundDatabaseError> {
        let migrations = crate::ProductionMigrations::load()?;
        super::apply_bound_with_clock(
            &self.path,
            backup_dir,
            engine_version,
            migrations.as_slice().as_slice(),
            clock,
            &self.handle,
        )
        .map_err(|error| match error {
            MigrationError::PathIdentityChanged => BoundDatabaseError::PathIdentityChanged,
            MigrationError::FileIdentity(source) => BoundDatabaseError::Identity(source),
            other => BoundDatabaseError::Migration(other),
        })
    }

    pub fn open_store(
        &self,
        engine_version: &str,
    ) -> Result<crate::EventStore, BoundDatabaseError> {
        let ids = crate::OsRandomIds::seed().map_err(crate::WriteError::from)?;
        match crate::EventStore::open_existing_with_ids(
            &self.path,
            &self.handle,
            engine_version,
            &ids,
        ) {
            Ok(store) => Ok(store),
            Err(crate::write::ExistingOpenError::Identity(source)) => {
                Err(BoundDatabaseError::Identity(source))
            }
            Err(crate::write::ExistingOpenError::Replaced) => {
                Err(BoundDatabaseError::PathIdentityChanged)
            }
            Err(crate::write::ExistingOpenError::Sqlite(error)) => {
                Err(BoundDatabaseError::Open(crate::WriteError::Sqlite(error)))
            }
            Err(crate::write::ExistingOpenError::Open(error)) => {
                Err(BoundDatabaseError::Open(error))
            }
        }
    }
}

pub(super) fn validate_bound_path(
    path: &Path,
    handle: &DatabaseFileHandle,
) -> Result<(), MigrationError> {
    handle.revalidate().map_err(MigrationError::FileIdentity)?;
    let observed = database_file_identity(path).map_err(MigrationError::FileIdentity)?;
    if observed != *handle.identity() {
        return Err(MigrationError::PathIdentityChanged);
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub(super) struct AuthorizedMigration<'a> {
    pub(super) plan: &'a MigrationPlan,
    pub(super) handle: &'a DatabaseFileHandle,
}

#[derive(Clone, Copy)]
pub(super) enum WritableTarget<'a> {
    Path,
    Bound(&'a DatabaseFileHandle),
    Authorized(AuthorizedMigration<'a>),
}

impl<'a> WritableTarget<'a> {
    pub(super) fn handle(self) -> Option<&'a DatabaseFileHandle> {
        match self {
            Self::Path => None,
            Self::Bound(handle) => Some(handle),
            Self::Authorized(authorized) => Some(authorized.handle),
        }
    }

    pub(super) fn authorization(self) -> Option<AuthorizedMigration<'a>> {
        match self {
            Self::Authorized(authorized) => Some(authorized),
            Self::Path | Self::Bound(_) => None,
        }
    }
}

pub(crate) fn apply_authorized_with_clock(
    db_path: &Path,
    engine_version: &str,
    migrations: &[&dyn Migration],
    clock: &dyn ClockAuthority,
    plan: &MigrationPlan,
    handle: &DatabaseFileHandle,
) -> Result<MigrationOutcome, MigrationError> {
    super::apply_inner_with_authorization(
        db_path,
        &plan.backup.location,
        engine_version,
        migrations,
        clock,
        WritableTarget::Authorized(AuthorizedMigration { plan, handle }),
        super::Interrupt::None,
    )
}

fn pending_identities(pending: &[usize], migrations: &[&dyn Migration]) -> Vec<MigrationId> {
    pending
        .iter()
        .map(|index| migrations[*index].id())
        .collect()
}

pub(super) fn future_backup(path: &Path, metadata: &std::fs::Metadata) -> FutureBackup {
    use super::preflight::{
        BackupCapacity, BackupDirectory, BackupPermissions, CreateBackupFile, WriteBackupFile,
    };

    // `path` is the real file (see `real_database_path`), so the backup and
    // its capacity check land on that file's filesystem.
    let location = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let minimum_free_bytes = metadata.len();
    let directory = match std::fs::read_dir(&location) {
        Ok(_) => BackupDirectory::Readable,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BackupDirectory::Absent,
        Err(_) => BackupDirectory::Unreadable,
    };
    let directory_writable = std::fs::metadata(&location)
        .map(|value| !value.permissions().readonly())
        .unwrap_or(false);
    let capacity = match directory {
        BackupDirectory::Readable => match fs2::available_space(&location) {
            Ok(available) if available >= minimum_free_bytes => BackupCapacity::Sufficient,
            Ok(_) => BackupCapacity::Insufficient,
            Err(_) => BackupCapacity::Unreadable,
        },
        BackupDirectory::Absent | BackupDirectory::Unreadable => BackupCapacity::Unreadable,
    };
    let create_file = if directory_writable {
        CreateBackupFile::Authorized
    } else {
        CreateBackupFile::Refused
    };
    let write_file = if directory_writable {
        WriteBackupFile::Authorized
    } else {
        WriteBackupFile::Refused
    };
    FutureBackup {
        location,
        minimum_free_bytes,
        capacity,
        directory,
        permissions: BackupPermissions {
            create_file,
            write_file,
        },
    }
}

fn backup_is_authorized(backup: &FutureBackup) -> bool {
    use super::preflight::{BackupCapacity, BackupDirectory, CreateBackupFile, WriteBackupFile};
    matches!(backup.directory, BackupDirectory::Readable)
        && matches!(backup.permissions.create_file, CreateBackupFile::Authorized)
        && matches!(backup.permissions.write_file, WriteBackupFile::Authorized)
        && matches!(backup.capacity, BackupCapacity::Sufficient)
}

pub(super) fn validate_before_sqlite_open(
    path: &Path,
    authorized: AuthorizedMigration<'_>,
) -> Result<(), MigrationError> {
    authorized
        .handle
        .revalidate()
        .map_err(MigrationError::FileIdentity)?;
    let observed = database_file_identity(path).map_err(MigrationError::FileIdentity)?;
    if observed != *authorized.handle.identity() {
        return Err(MigrationError::PathIdentityChanged);
    }
    let metadata = path.metadata().map_err(MigrationError::FileIdentity)?;
    let backup = future_backup(path, &metadata);
    if backup != authorized.plan.backup {
        return Err(MigrationError::AuthorizedPlanStale);
    }
    if !backup_is_authorized(&backup) {
        return Err(MigrationError::BackupAuthorityRefused);
    }
    Ok(())
}

pub(super) fn validate_authorization(
    path: &Path,
    migrations: &[&dyn Migration],
    pending: &[usize],
    authorized: AuthorizedMigration<'_>,
) -> Result<(), MigrationError> {
    let identity = database_file_identity(path).map_err(MigrationError::FileIdentity)?;
    authorized
        .handle
        .revalidate()
        .map_err(MigrationError::FileIdentity)?;
    if identity != *authorized.handle.identity() {
        return Err(MigrationError::PathIdentityChanged);
    }
    let metadata = path.metadata().map_err(MigrationError::FileIdentity)?;
    let observed_backup = future_backup(path, &metadata);
    if pending_identities(pending, migrations) != authorized.plan.pending
        || observed_backup != authorized.plan.backup
    {
        return Err(MigrationError::AuthorizedPlanStale);
    }
    if !backup_is_authorized(&observed_backup) {
        return Err(MigrationError::BackupAuthorityRefused);
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct ReadinessObservation {
    pub readiness: StoreReadiness,
    pub handle: Option<DatabaseFileHandle>,
}

pub(crate) fn preflight_observed(path: &Path) -> ReadinessObservation {
    preflight_with_validation(path, super::preflight::Validation::WholeDatabase)
}

/// A launch-time classification whose page walk is capped by the launch budget.
pub(crate) fn preflight_bounded_observed(path: &Path) -> ReadinessObservation {
    preflight_with_validation(path, super::preflight::Validation::LAUNCH_WALK)
}

/// The recovery classification with a witness-sized walk bound.
///
/// Only tests call this: it drives the same recovery path with a bound small
/// enough that a fixture can spend it, which the production budget cannot be
/// made small enough to do.
pub(crate) fn preflight_bounded_observation(path: &Path, operations: i64) -> ReadinessObservation {
    preflight_with_validation(
        path,
        super::preflight::Validation::launch_walk_within(operations),
    )
}

pub(crate) fn preflight_metadata_observed(path: &Path) -> ReadinessObservation {
    preflight_with_validation(path, super::preflight::Validation::Metadata)
}

fn preflight_with_validation(
    path: &Path,
    validation: super::preflight::Validation,
) -> ReadinessObservation {
    let before = database_file_handle(path).ok();
    let readiness = super::preflight::preflight_unfenced(path, validation);
    let after = database_file_identity(path).ok();
    if before.as_ref().map(DatabaseFileHandle::identity) != after.as_ref()
        && !matches!(readiness, StoreReadiness::Absent)
    {
        return ReadinessObservation {
            readiness: StoreReadiness::Unreadable(Unreadable::Replaced),
            handle: None,
        };
    }
    ReadinessObservation {
        readiness,
        handle: before,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration::preflight::{
        BackupCapacity, BackupDirectory, BackupPermissions, CreateBackupFile, WriteBackupFile,
    };

    fn backup(
        directory: BackupDirectory,
        capacity: BackupCapacity,
        create_file: CreateBackupFile,
        write_file: WriteBackupFile,
    ) -> FutureBackup {
        FutureBackup {
            location: std::path::PathBuf::from("backups"),
            minimum_free_bytes: 8,
            capacity,
            directory,
            permissions: BackupPermissions {
                create_file,
                write_file,
            },
        }
    }

    #[test]
    fn backup_authority_keeps_every_refusal_distinct() {
        assert!(!backup_is_authorized(&backup(
            BackupDirectory::Absent,
            BackupCapacity::Unreadable,
            CreateBackupFile::Refused,
            WriteBackupFile::Refused,
        )));
        assert!(!backup_is_authorized(&backup(
            BackupDirectory::Unreadable,
            BackupCapacity::Unreadable,
            CreateBackupFile::Refused,
            WriteBackupFile::Refused,
        )));
        assert!(!backup_is_authorized(&backup(
            BackupDirectory::Readable,
            BackupCapacity::Sufficient,
            CreateBackupFile::Refused,
            WriteBackupFile::Authorized,
        )));
        assert!(!backup_is_authorized(&backup(
            BackupDirectory::Readable,
            BackupCapacity::Sufficient,
            CreateBackupFile::Authorized,
            WriteBackupFile::Refused,
        )));
        assert!(!backup_is_authorized(&backup(
            BackupDirectory::Readable,
            BackupCapacity::Insufficient,
            CreateBackupFile::Authorized,
            WriteBackupFile::Authorized,
        )));
        assert!(backup_is_authorized(&backup(
            BackupDirectory::Readable,
            BackupCapacity::Sufficient,
            CreateBackupFile::Authorized,
            WriteBackupFile::Authorized,
        )));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_binding_tests;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod sqlite_target_tests;
