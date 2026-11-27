use crate::checkpoint::{Checkpoint, Wal, WalState, TICK};
use crate::command::{Command, Durability, Receipt, MAX_COMMAND_BYTES};
use crate::error::{OpenError, WriteError};
use crate::key::RequestKey;
use crate::migration::{self, Migrated, MigrationSpec};
use crate::read::ReadPool;
use crate::wal_budget::WalBudget;
use crate::{sqlite, transaction};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::sync::Arc;
use tokio::sync::{broadcast, oneshot, OwnedSemaphorePermit};

const GROUP_REQUESTS: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitNotice {
    pub key: RequestKey,
}

pub(crate) struct Request {
    pub(crate) command: Command,
    pub(crate) reply: oneshot::Sender<Result<Receipt, WriteError>>,
    pub(crate) slot: OwnedSemaphorePermit,
    pub(crate) bytes: OwnedSemaphorePermit,
}

impl Request {
    fn reply(self, outcome: Result<Receipt, WriteError>) {
        let Self {
            command,
            reply,
            slot,
            bytes,
        } = self;
        drop(command);
        let _ = reply.send(outcome);
        drop((slot, bytes));
    }
}

pub(crate) fn run(
    path: PathBuf,
    ledger: &'static [MigrationSpec],
    queue: Receiver<Request>,
    ready: oneshot::Sender<Result<(ReadPool, Migrated), OpenError>>,
    state: Arc<WalState>,
    notices: broadcast::Sender<CommitNotice>,
) -> Result<(), OpenError> {
    let (path, _writer_lease, mut connection, migrated) = match initialize(&path, ledger) {
        Ok(value) => value,
        Err(error) => {
            let _ = ready.send(Err(error));
            return Ok(());
        }
    };
    let budget = Arc::new(WalBudget::new(
        &connection,
        sqlite::sidecar(&path, "-wal"),
        &sqlite::INTERNAL,
    )?);
    let restricted = sqlite::install_guard(&mut connection, Arc::clone(&budget))?;
    let mut wal = Wal::new(budget, Arc::clone(&state));
    // Observe the file before admitting work, including a WAL recovered on open.
    let _ = wal.observe(&connection);
    let checkpoint = Checkpoint::spawn(path.clone(), state)?;
    let readers = ReadPool::new(path);
    let mut executor = Executor {
        connection,
        restricted,
        wal,
        notices,
    };
    if ready.send(Ok((readers.clone(), migrated))).is_ok() {
        executor.drain(&queue);
    }
    // Close the checkpointer first so the writer owns SQLite's last writable
    // handle and any checkpoint SQLite performs on clean final close.
    readers.stop();
    let cache = readers.close_idle();
    checkpoint.finish().map_err(|_| OpenError::Worker)?;
    cache.map_err(|_| OpenError::Worker)
}

/// Lock, check the schema with reads only, migrate, then switch to WAL and
/// restrict. A refusal or failed backup leaves the file's content and journal
/// mode as found.
fn initialize(
    path: &std::path::Path,
    ledger: &[MigrationSpec],
) -> Result<(PathBuf, std::fs::File, Connection, Migrated), OpenError> {
    let (path, lease) = sqlite::lock_writer(path)?;
    let mut connection = sqlite::writer(&path)?;
    let plan = migration::plan(&connection, ledger)?;
    let migrated = migration::apply(&mut connection, &path, ledger, plan)?;
    sqlite::enable_wal(&connection)?;
    sqlite::restrict(&connection)?;
    Ok((path, lease, connection, migrated))
}

struct Executor {
    connection: Connection,
    restricted: Arc<sqlite::Guard>,
    wal: Wal,
    notices: broadcast::Sender<CommitNotice>,
}

impl Executor {
    fn drain(&mut self, queue: &Receiver<Request>) {
        let mut next = None;
        loop {
            let first = match next.take() {
                Some(request) => request,
                None => match queue.recv_timeout(TICK) {
                    Ok(request) => request,
                    Err(RecvTimeoutError::Timeout) => {
                        let _ = self.wal.observe(&self.connection);
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                },
            };
            let (group, deferred) = collect(first, queue);
            next = deferred;
            for batch in by_durability(group) {
                self.execute(batch);
            }
        }
    }

    fn execute(&mut self, batch: Vec<Request>) {
        match self.wal.observe(&self.connection) {
            Ok(false) => {}
            Ok(true) => return refuse(batch, || WriteError::WalPressure),
            Err(error) => {
                return refuse(batch, || {
                    WriteError::WalIo(std::io::Error::other(error.to_string()))
                })
            }
        }
        let Some(first) = batch.first() else {
            return;
        };
        let commands: Vec<_> = batch.iter().map(|request| &request.command).collect();
        let results = transaction::run(
            &mut self.connection,
            &self.restricted,
            first.command.durability,
            &commands,
        );
        self.wal.wrote();
        // Update the admission snapshot before any client can submit again.
        let _ = self.wal.observe(&self.connection);
        for (request, result) in batch.into_iter().zip(results) {
            let outcome = result.map(|(receipt, fresh)| {
                if fresh {
                    let _ = self.notices.send(CommitNotice {
                        key: request.command.key.clone(),
                    });
                }
                receipt
            });
            request.reply(outcome);
        }
    }
}

fn refuse(batch: Vec<Request>, error: impl Fn() -> WriteError) {
    for request in batch {
        request.reply(Err(error()));
    }
}

fn collect(first: Request, queue: &Receiver<Request>) -> (Vec<Request>, Option<Request>) {
    let mut bytes = first.command.bytes;
    let mut group = vec![first];
    while group.len() < GROUP_REQUESTS {
        match queue.try_recv() {
            Ok(request) if bytes + request.command.bytes <= MAX_COMMAND_BYTES => {
                bytes += request.command.bytes;
                group.push(request);
            }
            Ok(request) => return (group, Some(request)),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
    (group, None)
}

fn conflicts(left: &Command, right: &Command) -> bool {
    left.key == right.key || left.scope.intersects(&right.scope)
}

fn by_durability(group: Vec<Request>) -> Vec<Vec<Request>> {
    let mut first = Vec::new();
    let mut deferred: Vec<Request> = Vec::new();
    for request in group {
        let eligible = request.command.durability == Durability::Normal
            && !deferred
                .iter()
                .any(|prior| conflicts(&prior.command, &request.command));
        if eligible {
            first.push(request);
        } else {
            deferred.push(request);
        }
    }
    let mut batches = Vec::new();
    if !first.is_empty() {
        batches.push(first);
    }
    // Keep the rest in admission order. Only adjacent requests of the same
    // durability may share the outer transaction.
    let mut current = Vec::<Request>::new();
    for request in deferred {
        if current
            .last()
            .is_some_and(|prior| prior.command.durability != request.command.durability)
        {
            batches.push(std::mem::take(&mut current));
        }
        current.push(request);
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}
