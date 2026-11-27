use grapt_types::MigrationId;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationPlan {
    pub pending: Vec<MigrationId>,
    pub backup: FutureBackup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FutureBackup {
    pub location: PathBuf,
    pub minimum_free_bytes: u64,
    pub capacity: BackupCapacity,
    pub directory: BackupDirectory,
    pub permissions: BackupPermissions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupCapacity {
    Sufficient,
    Insufficient,
    Unreadable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupDirectory {
    Readable,
    Absent,
    Unreadable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupPermissions {
    pub create_file: CreateBackupFile,
    pub write_file: WriteBackupFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateBackupFile {
    Authorized,
    Refused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteBackupFile {
    Authorized,
    Refused,
}
