use crate::key::{InvalidRequestKey, RequestKey};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("run store already has a writer")]
    WriterExists,
    #[error("run store did not start or shut down cleanly")]
    Worker,
    #[error("unsupported run store schema version {0}")]
    Schema(i64),
    #[error("run store requires auto_vacuum=NONE for checked WAL commits")]
    AutoVacuum,
    #[error("run store row or index layout exceeds the checked WAL reserve")]
    WalLayout,
    #[error("could not enable WAL journal mode: {0}")]
    JournalMode(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
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
