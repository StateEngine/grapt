use crate::config::ActorConfig;
use crate::error::{SpawnError, SubmitError};
use crate::key::SubmitKey;
use crate::state::{Applied, Event, RuntimeSnapshot, StateChange};
use crate::store;
use crate::worker::{Request, Worker};
use grapt_run_store::{ReadPool, Writer};
use grapt_types::SessionId;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// Submits commands to one session actor and observes its committed state.
/// Handles hold no sender of their own, so every subscription closes when
/// the actor's task ends.
#[derive(Debug)]
pub struct ActorHandle {
    mailbox: mpsc::Sender<Request>,
    snapshots: watch::Receiver<RuntimeSnapshot>,
    changes: broadcast::Receiver<StateChange>,
}

impl Clone for ActorHandle {
    fn clone(&self) -> Self {
        Self {
            mailbox: self.mailbox.clone(),
            snapshots: self.snapshots.clone(),
            changes: self.changes.resubscribe(),
        }
    }
}

impl ActorHandle {
    /// Admission never waits: a full mailbox is refused at once.
    pub async fn submit(&self, key: SubmitKey, event: Event) -> Result<Applied, SubmitError> {
        let (reply, outcome) = oneshot::channel();
        self.mailbox
            .try_send(Request { key, event, reply })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => SubmitError::MailboxFull,
                mpsc::error::TrySendError::Closed(_) => SubmitError::Closed,
            })?;
        // An actor that stops without replying may have committed the write.
        outcome.await.map_err(|_| SubmitError::OutcomeUnknown)?
    }

    /// The latest committed snapshot.
    pub fn snapshot(&self) -> RuntimeSnapshot {
        self.snapshots.borrow().clone()
    }

    /// Committed snapshots as they change. This is the source of truth for a
    /// subscriber that lagged on changes.
    pub fn watch(&self) -> watch::Receiver<RuntimeSnapshot> {
        self.snapshots.clone()
    }

    /// Committed transitions. A reload publishes only a snapshot, so a gap in
    /// `state_seq` means: read [`Self::snapshot`].
    pub fn subscribe(&self) -> broadcast::Receiver<StateChange> {
        self.changes.resubscribe()
    }
}

/// Owns the task that serializes one session's transitions.
#[derive(Debug)]
pub struct SessionActor {
    handle: ActorHandle,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<(), SpawnError>>>,
}

impl SessionActor {
    /// Load the session's committed state and start its actor.
    pub async fn spawn(
        writer: Writer,
        readers: ReadPool,
        session: SessionId,
        config: ActorConfig,
    ) -> Result<Self, SpawnError> {
        let current = store::load(&readers, session, Instant::now() + config.read_timeout).await?;
        let (mailbox, requests) = mpsc::channel(config.mailbox.max(1));
        let (snapshots, observed) = watch::channel(current.clone());
        let (changes, template) = broadcast::channel(config.changes.max(1));
        let (stop, stopped) = oneshot::channel();
        let worker = Worker::new(writer, readers, current, snapshots, changes, config);
        let task = tokio::spawn(worker.run(requests, stopped));
        Ok(Self {
            handle: ActorHandle {
                mailbox,
                snapshots: observed,
                changes: template,
            },
            stop: Some(stop),
            task: Some(task),
        })
    }

    pub fn handle(&self) -> ActorHandle {
        self.handle.clone()
    }

    /// Finish the command in progress, refuse queued ones as closed, settle
    /// any write whose outcome is unknown, and wait for the task to exit.
    pub async fn shutdown(mut self) -> Result<(), SpawnError> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        match self.task.take() {
            Some(task) => task.await.map_err(|_| SpawnError::Worker)?,
            None => Ok(()),
        }
    }
}

impl Drop for SessionActor {
    /// The task ends on its own after its current command; it is not aborted
    /// mid-write.
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}
