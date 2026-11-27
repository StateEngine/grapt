use crate::error::{OpenError, WriteError};
use crate::wal_budget::WalBudget;
use rusqlite::{Connection, OpenFlags};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tokio::time::Instant;

pub(crate) const TICK: Duration = Duration::from_millis(25);
const RESET_RETRY: Duration = Duration::from_millis(200);

#[derive(Debug, Default)]
pub(crate) struct WalState {
    pub(crate) pressure: AtomicBool,
    pub(crate) bytes: AtomicU64,
    pub(crate) peak: AtomicU64,
    generation: AtomicU64,
    copied: AtomicU64,
    stop: AtomicBool,
    pub(crate) copy_failures: AtomicU64,
    pub(crate) checkpoint_error: Mutex<Option<String>>,
}

pub(crate) struct Checkpoint {
    state: Arc<WalState>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Checkpoint {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Checkpoint {
    pub(crate) fn spawn(path: PathBuf, state: Arc<WalState>) -> Result<Self, OpenError> {
        let worker = Arc::clone(&state);
        let (ready, started) = std::sync::mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("run-store-checkpoint".into())
            .spawn(move || match connect(&path) {
                Ok(connection) => {
                    if ready.send(Ok(())).is_ok() {
                        copy(&connection, &worker);
                    }
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                }
            })?;
        started.recv().map_err(|_| OpenError::Worker)??;
        Ok(Self {
            state,
            thread: Some(thread),
        })
    }

    pub(crate) fn finish(mut self) -> thread::Result<()> {
        self.stop();
        self.thread.take().map_or(Ok(()), JoinHandle::join)
    }

    fn stop(&self) {
        self.state.stop.store(true, Ordering::Release);
        if let Some(thread) = &self.thread {
            thread.thread().unpark();
        }
    }
}

fn connect(path: &std::path::Path) -> rusqlite::Result<Connection> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    connection.busy_timeout(Duration::ZERO)?;
    connection.execute_batch("PRAGMA temp_store=FILE; PRAGMA wal_autocheckpoint=0;")?;
    Ok(connection)
}

fn copy(connection: &Connection, state: &WalState) {
    while !state.stop.load(Ordering::Acquire) {
        let generation = state.generation.load(Ordering::Acquire);
        if state.copied.load(Ordering::Acquire) != generation {
            match status(connection, "PRAGMA wal_checkpoint(PASSIVE)") {
                Ok((0, log, done)) if log >= 0 && log == done => {
                    state.copied.store(generation, Ordering::Release);
                }
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.sqlite_error_code(),
                        Some(
                            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                        )
                    ) => {}
                Err(error) => {
                    state.copy_failures.fetch_add(1, Ordering::Relaxed);
                    if let Ok(mut last) = state.checkpoint_error.lock() {
                        *last = Some(error.to_string());
                    }
                }
            }
        }
        thread::park_timeout(TICK);
    }
}

fn status(connection: &Connection, sql: &str) -> rusqlite::Result<(i64, i64, i64)> {
    connection.query_row(sql, [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
}

pub(crate) struct Wal {
    budget: Arc<WalBudget>,
    state: Arc<WalState>,
    next_reset: Instant,
}

impl Wal {
    pub(crate) fn new(budget: Arc<WalBudget>, state: Arc<WalState>) -> Self {
        state.generation.store(1, Ordering::Release);
        Self {
            budget,
            state,
            next_reset: Instant::now(),
        }
    }

    pub(crate) fn wrote(&self) {
        self.state.generation.fetch_add(1, Ordering::Release);
    }

    pub(crate) fn observe(&mut self, connection: &Connection) -> Result<bool, WriteError> {
        let mut bytes = self.budget.size()?;
        self.state.peak.fetch_max(bytes, Ordering::Relaxed);
        if self.budget.at_limit(bytes)
            && Instant::now() >= self.next_reset
            && self.state.generation.load(Ordering::Acquire)
                == self.state.copied.load(Ordering::Acquire)
        {
            self.next_reset = Instant::now() + RESET_RETRY;
            let (busy, _, _) = status(connection, "PRAGMA wal_checkpoint(TRUNCATE)")
                .map_err(WriteError::RolledBack)?;
            if busy == 0 {
                bytes = self.budget.size()?;
            }
        }
        let pressure = self.budget.at_limit(bytes);
        self.state.bytes.store(bytes, Ordering::Relaxed);
        self.state.pressure.store(pressure, Ordering::Release);
        Ok(pressure)
    }
}
