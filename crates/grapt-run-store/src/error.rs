use crate::key::{InvalidRequestKey, RequestKey};
use std::sync::Arc;

type Cause = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("run store already has a writer")]
    WriterExists,
    #[error("run store did not start or shut down cleanly")]
    Worker,
    #[error("file is not a Grapt run store (schema version {0}); choose another path")]
    Schema(i64),
    #[error(
        "run store schema version {found} is newer than the {max_supported} this build knows; \
         open it with a newer Grapt"
    )]
    FutureSchemaVersion { found: i64, max_supported: u32 },
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error("migration {version} failed and was rolled back; the database was not changed")]
    Migration {
        version: u32,
        #[source]
        source: rusqlite::Error,
    },
    #[error("could not take a verified backup before migrating; the database was not changed")]
    BackupFailed(#[source] Box<OpenError>),
    /// Reported only as the source of [`OpenError::BackupFailed`].
    #[error("backup copy failed verification: {0}")]
    BackupUnverified(String),
    #[error("the disk holding the run store is full; free space and open it again")]
    DiskFull(#[source] Cause),
    #[error("another process held the run store's lock too long; close it and open again")]
    StorageStalled(#[source] Cause),
    #[error("run store requires auto_vacuum=NONE for checked WAL commits")]
    AutoVacuum,
    #[error("run store row or index layout exceeds the checked WAL reserve")]
    WalLayout,
    #[error("could not enable WAL journal mode: {0}")]
    JournalMode(String),
    #[error(transparent)]
    Io(std::io::Error),
    #[error(transparent)]
    Sqlite(rusqlite::Error),
}

/// Storage pressure is named at the open boundary; anything else stays raw.
impl From<rusqlite::Error> for OpenError {
    fn from(error: rusqlite::Error) -> Self {
        use rusqlite::ErrorCode;
        match error.sqlite_error_code() {
            Some(ErrorCode::DiskFull) => Self::DiskFull(Box::new(error)),
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => {
                Self::StorageStalled(Box::new(error))
            }
            _ => Self::Sqlite(error),
        }
    }
}

impl From<std::io::Error> for OpenError {
    fn from(error: std::io::Error) -> Self {
        use std::io::ErrorKind;
        match error.kind() {
            ErrorKind::StorageFull | ErrorKind::QuotaExceeded => Self::DiskFull(Box::new(error)),
            _ => Self::Io(error),
        }
    }
}

/// The schema ledger a build supplies, or the one a database recorded, cannot
/// be trusted.
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("migration ledger is invalid: {0}")]
    Spec(&'static str),
    #[error("run store has no migration ledger; it was not created by a supported build")]
    Missing,
    #[error("run store ledger does not match its schema version at migration {version}")]
    Inconsistent { version: i64 },
    #[error("migration {version} differs from the one this database applied")]
    Changed { version: u32 },
    #[error("migration {version} ended the open transaction; remove BEGIN or COMMIT from it")]
    Transaction { version: u32 },
}

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error(transparent)]
    InvalidKey(#[from] InvalidRequestKey),
    #[error("write command exceeds its input or statement budget")]
    CommandTooLarge,
    #[error("writer queue is full; retry with the same request key")]
    QueueFull,
    #[error("writer byte budget is full; retry with the same request key")]
    ByteBudget,
    #[error(
        "WAL or savepoint capacity reached; retry a smaller write or wait for readers to release their snapshots"
    )]
    WalPressure,
    #[error("write requires temporary query storage; use an indexed or streaming write")]
    TemporaryStorage,
    #[error("write exceeds the SQLite heap budget; retry with smaller expressions")]
    HeapPressure,
    #[error("writer is closed")]
    Closed,
    #[error("deadline expired before admission")]
    NotAdmitted,
    #[error("outcome is unresolved for {key}; retry with the same request key")]
    OutcomeUnknown {
        key: RequestKey,
        #[source]
        cause: Option<Arc<rusqlite::Error>>,
    },
    #[error("request key is already bound to different input")]
    KeyConflict,
    #[error("statement affected {actual} rows; expected {expected}")]
    RowCount { expected: u64, actual: u64 },
    #[error("durable request receipt is malformed")]
    Receipt,
    #[error("could not observe WAL capacity: {0}")]
    WalIo(#[source] std::io::Error),
    #[error("write group was aborted: {0}")]
    BatchAborted(#[source] Arc<rusqlite::Error>),
    #[error("write was rolled back: {0}")]
    RolledBack(#[source] rusqlite::Error),
}

impl WriteError {
    /// The SQLite error that decided this outcome, when it reached SQLite.
    pub fn sqlite_cause(&self) -> Option<&rusqlite::Error> {
        match self {
            Self::OutcomeUnknown { cause, .. } => cause.as_deref(),
            Self::BatchAborted(error) => Some(error),
            Self::RolledBack(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("read capacity or snapshot deadline expired")]
    Busy,
    #[error("read pool is closed")]
    Closed,
    #[error("read worker stopped")]
    Worker,
    #[error("durable request receipt is malformed")]
    Receipt,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}
