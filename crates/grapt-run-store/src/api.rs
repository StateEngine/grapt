use crate::checkpoint::WalState;
use crate::command::{Command, Receipt};
use crate::error::{OpenError, WriteError};
use crate::read::ReadPool;
use crate::writer::{self, Request};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use tokio::sync::{broadcast, oneshot, Semaphore};
use tokio::time::{timeout_at, Instant};

pub use crate::writer::CommitNotice;

const SLOTS: usize = 256;
const BYTE_UNIT: usize = 1024;
const BYTE_PERMITS: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub struct Writer(Arc<Admission>);

#[derive(Debug)]
struct Admission {
    sender: Mutex<Option<SyncSender<Request>>>,
    slots: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
    wal: Arc<WalState>,
    notices: broadcast::Sender<CommitNotice>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoreStats {
    pub admitted_requests: usize,
    pub admitted_bytes: usize,
    pub wal_bytes: u64,
    pub wal_peak_bytes: u64,
    pub wal_pressure: bool,
    pub checkpoint_failures: u64,
}

#[derive(Debug)]
pub struct RunStore {
    writer: Writer,
    readers: ReadPool,
    worker: Option<JoinHandle<Result<(), OpenError>>>,
}

impl RunStore {
    /// Opens a standalone run database or initializes it with trusted,
    /// compiled domain DDL. The schema is used only on first open.
    pub async fn open(path: impl AsRef<Path>, schema: &'static str) -> Result<Self, OpenError> {
        let path = path.as_ref().to_owned();
        let (sender, queue) = sync_channel(SLOTS);
        let (ready, started) = oneshot::channel();
        let wal = Arc::new(WalState::default());
        let worker_wal = Arc::clone(&wal);
        let (notices, _) = broadcast::channel(SLOTS);
        let sink = notices.clone();
        let worker = thread::Builder::new()
            .name("run-store-writer".into())
            .spawn(move || writer::run(path, schema, queue, ready, worker_wal, sink))?;
        let readers = match started.await {
            Ok(result) => result?,
            Err(_) => {
                return match join(worker).await {
                    Ok(()) => Err(OpenError::Worker),
                    Err(error) => Err(error),
                };
            }
        };
        Ok(Self {
            writer: Writer(Arc::new(Admission {
                sender: Mutex::new(Some(sender)),
                slots: Arc::new(Semaphore::new(SLOTS)),
                bytes: Arc::new(Semaphore::new(BYTE_PERMITS)),
                wal,
                notices,
            })),
            readers,
            worker: Some(worker),
        })
    }

    pub fn writer(&self) -> Writer {
        self.writer.clone()
    }

    pub fn readers(&self) -> ReadPool {
        self.readers.clone()
    }

    /// Stop new writes, drain accepted writes and close all SQLite handles.
    pub async fn shutdown(mut self) -> Result<(), OpenError> {
        self.writer.close();
        let readers = self.readers.close().await;
        if let Some(worker) = self.worker.take() {
            join(worker).await?;
        }
        readers.map_err(|_| OpenError::Worker)
    }
}

async fn join(worker: JoinHandle<Result<(), OpenError>>) -> Result<(), OpenError> {
    tokio::task::spawn_blocking(move || worker.join().map_err(|_| OpenError::Worker)?)
        .await
        .map_err(|_| OpenError::Worker)?
}

impl Drop for RunStore {
    fn drop(&mut self) {
        self.writer.close();
        self.readers.stop();
    }
}

impl Writer {
    /// A deadline after acceptance or a lost reply leaves an unresolved key.
    /// Only a receipt can establish success, and retry must reuse the key.
    pub async fn write(&self, command: Command, deadline: Instant) -> Result<Receipt, WriteError> {
        let key = command.key.clone();
        let bytes = command.bytes.div_ceil(BYTE_UNIT) as u32;
        let (reply, receive) = oneshot::channel();
        {
            let sender = self.0.sender.lock().map_err(|_| WriteError::Closed)?;
            let sender = sender.as_ref().ok_or(WriteError::Closed)?;
            if Instant::now() >= deadline {
                return Err(WriteError::NotAdmitted);
            }
            if self.0.wal.pressure.load(Ordering::Acquire) {
                return Err(WriteError::WalPressure);
            }
            let slot = Arc::clone(&self.0.slots)
                .try_acquire_owned()
                .map_err(|_| WriteError::QueueFull)?;
            let bytes = Arc::clone(&self.0.bytes)
                .try_acquire_many_owned(bytes)
                .map_err(|_| WriteError::ByteBudget)?;
            sender
                .try_send(Request {
                    command,
                    reply,
                    slot,
                    bytes,
                })
                .map_err(|error| match error {
                    TrySendError::Full(_) => WriteError::QueueFull,
                    TrySendError::Disconnected(_) => WriteError::Closed,
                })?;
        }
        match timeout_at(deadline, receive).await {
            Ok(Ok(result)) => result,
            _ => Err(WriteError::OutcomeUnknown { key, cause: None }),
        }
    }

    /// Receivers that lag must read durable state before consuming more notices.
    pub fn subscribe(&self) -> broadcast::Receiver<CommitNotice> {
        self.0.notices.subscribe()
    }

    pub fn stats(&self) -> StoreStats {
        StoreStats {
            admitted_requests: SLOTS - self.0.slots.available_permits(),
            admitted_bytes: (BYTE_PERMITS - self.0.bytes.available_permits()) * BYTE_UNIT,
            wal_bytes: self.0.wal.bytes.load(Ordering::Relaxed),
            wal_peak_bytes: self.0.wal.peak.load(Ordering::Relaxed),
            wal_pressure: self.0.wal.pressure.load(Ordering::Acquire),
            checkpoint_failures: self.0.wal.copy_failures.load(Ordering::Relaxed),
        }
    }

    /// Last checkpoint error, retained across subsequent successful copies.
    pub fn last_checkpoint_error(&self) -> Option<String> {
        self.0
            .wal
            .checkpoint_error
            .lock()
            .ok()
            .and_then(|error| error.clone())
    }

    fn close(&self) {
        if let Ok(mut sender) = self.0.sender.lock() {
            let _ = sender.take();
        }
    }
}
