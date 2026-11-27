//! Bounded independent WAL snapshots for finite synchronous projections.
use crate::migration::authority::{
    database_file_handle, database_file_identity, DatabaseFileHandle,
};
use crate::{EventStore, WriteError};
use rusqlite::{ErrorCode, OpenFlags};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// The finite readers one pool lends at once.
pub const READ_CAPACITY: usize = 16;
/// How long a checkout waits for a free reader, and a reader for SQLite,
/// before the caller sees `Busy`; planning treats that as a capacity wait.
pub const READ_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum ReadPoolError {
    #[error("read capacity is busy; retry the request")]
    Busy,
    #[error("read pool mutex is poisoned")]
    Poisoned,
    #[error("database file identity: {0}")]
    Identity(#[from] std::io::Error),
    #[error("database path no longer names the open database")]
    Replaced,
    #[error("independent readers require WAL; database journal mode is {0}")]
    JournalMode(String),
    #[error(transparent)]
    Store(WriteError),
    #[error("read database: {0}")]
    Sqlite(rusqlite::Error),
}

impl From<rusqlite::Error> for ReadPoolError {
    fn from(error: rusqlite::Error) -> Self {
        match error.sqlite_error_code() {
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => Self::Busy,
            _ => Self::Sqlite(error),
        }
    }
}

impl From<WriteError> for ReadPoolError {
    fn from(error: WriteError) -> Self {
        match error {
            WriteError::Sqlite(error) | WriteError::Epoch(crate::EpochError::Sqlite(error)) => {
                error.into()
            }
            error => Self::Store(error),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReadPool {
    inner: Arc<Pool>,
}

#[derive(Debug)]
struct Pool {
    path: PathBuf,
    file: DatabaseFileHandle,
    state: Mutex<State>,
    available: Condvar,
}

#[derive(Debug, Default)]
struct State {
    used: usize,
    idle: Vec<EventStore>,
}

/// A reservation is returned even when opening fails or a projection unwinds.
#[derive(Debug)]
struct Reservation {
    pool: ReadPool,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut state = self
            .pool
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.used -= 1;
        self.pool.inner.available.notify_one();
    }
}

/// Keep a lease inside one blocking job, never across a network wait or await.
/// Finishing ends its transaction before caching the connection, so an idle
/// reader cannot retain a WAL pin and starve checkpoints. Unwinding closes it.
#[derive(Debug)]
#[must_use = "finish the finite projection before returning its reader capacity"]
pub struct ReadLease {
    store: EventStore,
    reservation: Reservation,
}

impl EventStore {
    /// Share one pool per writer. Anonymous memory stores keep their single
    /// connection; production file stores always use independent readers.
    pub fn read_pool(&self) -> Result<Option<ReadPool>, ReadPoolError> {
        if let Some(pool) = self.readers.get() {
            return Ok(pool.clone());
        }
        let file: String = self.connection().query_row(
            "SELECT file FROM pragma_database_list WHERE name='main'",
            [],
            |row| row.get(0),
        )?;
        let pool = if file.is_empty() {
            None
        } else {
            let path = PathBuf::from(file);
            Some(ReadPool {
                inner: Arc::new(Pool {
                    file: database_file_handle(&path)?,
                    path,
                    state: Mutex::new(State::default()),
                    available: Condvar::new(),
                }),
            })
        };
        let _ = self.readers.set(pool);
        Ok(self.readers.get().cloned().flatten())
    }
}

impl ReadPool {
    fn validate(&self) -> Result<(), ReadPoolError> {
        self.inner.file.revalidate()?;
        if database_file_identity(&self.inner.path)? != *self.inner.file.identity() {
            return Err(ReadPoolError::Replaced);
        }
        Ok(())
    }

    pub fn checkout(&self) -> Result<ReadLease, ReadPoolError> {
        self.checkout_within(|| READ_WAIT)
    }

    /// Check out a reader, waiting for a free one and for SQLite while pinning
    /// it no longer than `remaining` reports (capped at [`READ_WAIT`]). The
    /// caller owns the clock, so one with its own shorter deadline never
    /// leaves a blocking worker waiting after it has answered.
    pub fn checkout_within(
        &self,
        remaining: impl Fn() -> Duration,
    ) -> Result<ReadLease, ReadPoolError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| ReadPoolError::Poisoned)?;
        let (mut state, _) = self
            .inner
            .available
            .wait_timeout_while(state, remaining().min(READ_WAIT), |state| {
                state.used == READ_CAPACITY
            })
            .map_err(|_| ReadPoolError::Poisoned)?;
        if state.used == READ_CAPACITY {
            return Err(ReadPoolError::Busy);
        }
        state.used += 1;
        let idle = state.idle.pop();
        drop(state);
        let reservation = Reservation { pool: self.clone() };
        self.validate()?;
        let store = match idle {
            Some(store) => store,
            None => self.connect(remaining().min(READ_WAIT))?,
        };
        store
            .connection()
            .busy_timeout(remaining().min(READ_WAIT))?;
        store.connection().execute_batch("BEGIN DEFERRED")?;
        // Pin the database now, before a projection makes its first query.
        if crate::validate_read_only(store.connection())
            .map_err(WriteError::from)?
            .is_none()
        {
            return Err(WriteError::Epoch(crate::EpochError::CorruptAttribution {
                reason: "reader requires an initialized engine database".into(),
            })
            .into());
        }
        self.validate()?;
        // Once pinned, the projection's own queries keep the full wait.
        store.connection().busy_timeout(READ_WAIT)?;
        Ok(ReadLease { store, reservation })
    }

    fn connect(&self, busy: Duration) -> Result<EventStore, ReadPoolError> {
        let connection = self
            .inner
            .file
            .open_sqlite(&self.inner.path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_timeout(busy)?;
        connection.pragma_update(None, "query_only", true)?;
        let journal: String =
            connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        if journal != "wal" {
            return Err(ReadPoolError::JournalMode(journal));
        }
        let ids = crate::OsRandomIds::seed().map_err(WriteError::from)?;
        // Install normal query guards without schema preparation or election.
        let store = EventStore::guarded(connection, &ids)?;
        // A read inbox is already inside this pool's pin. Nested projections
        // reuse it instead of opening another pool or caching a cycle.
        let _ = store.readers.set(None);
        Ok(store)
    }
}

impl ReadLease {
    pub fn store(&self) -> &EventStore {
        &self.store
    }
    pub fn store_mut(&mut self) -> &mut EventStore {
        &mut self.store
    }

    pub fn finish(self) -> Result<(), ReadPoolError> {
        let Self { store, reservation } = self;
        if let Err(error) = store.connection().execute_batch("ROLLBACK") {
            drop(store);
            return Err(error.into());
        }
        if let Err(error) = reservation.pool.validate() {
            drop(store);
            return Err(error);
        }
        reservation
            .pool
            .inner
            .state
            .lock()
            .map_err(|_| ReadPoolError::Poisoned)?
            .idle
            .push(store);
        Ok(())
    }

    /// Return the capacity without returning the connection.
    ///
    /// A reader whose projection failed cannot prove the connection still
    /// reaches the database: an I/O error can leave it pinned to a page it can
    /// no longer read while the transaction itself still rolls back. Idling it
    /// would hand that state to the next checkout, which then fails for a
    /// reason it never saw. Closing it costs one open on recovery and keeps the
    /// failure with the read that met it.
    pub fn discard(self) {
        // Dropping the store closes the connection; dropping the reservation
        // returns the capacity and wakes one waiter.
        drop(self);
    }
}
