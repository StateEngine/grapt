use crate::dedup::Recent;
use crate::error::{SpawnError, SubmitError};
use crate::key::SubmitKey;
use crate::state::{Applied, Event, RuntimeSnapshot, SessionRuntimeState, StateChange, StateSeq};
use crate::store;
use grapt_run_store::{ReadPool, WriteError, Writer};
use grapt_types::SessionId;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

#[derive(Clone, Copy, Debug)]
pub struct ActorConfig {
    /// Commands admitted but not yet started; more are refused.
    pub mailbox: usize,
    /// State changes retained for slow subscribers before they lag.
    pub changes: usize,
    /// Submit outcomes answered from memory on retry.
    pub recent_keys: usize,
    pub write_timeout: Duration,
    pub read_timeout: Duration,
}

impl Default for ActorConfig {
    fn default() -> Self {
        Self {
            mailbox: 64,
            changes: 256,
            recent_keys: 256,
            write_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(5),
        }
    }
}

struct Request {
    key: SubmitKey,
    event: Event,
    reply: oneshot::Sender<Result<Applied, SubmitError>>,
}

/// Submits commands to one session actor and observes its committed state.
#[derive(Clone, Debug)]
pub struct ActorHandle {
    mailbox: mpsc::Sender<Request>,
    snapshots: watch::Receiver<RuntimeSnapshot>,
    changes: broadcast::Sender<StateChange>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("key", &self.key)
            .field("event", &self.event)
            .finish_non_exhaustive()
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

    /// Committed transitions. A reload after an unknown outcome publishes only
    /// a snapshot, so a gap in `state_seq` means: read [`Self::snapshot`].
    pub fn subscribe(&self) -> broadcast::Receiver<StateChange> {
        self.changes.subscribe()
    }
}

/// Owns the task that serializes one session's transitions.
#[derive(Debug)]
pub struct SessionActor {
    handle: ActorHandle,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
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
        let (changes, _) = broadcast::channel(config.changes.max(1));
        let (stop, stopped) = oneshot::channel();
        let worker = Worker {
            writer,
            readers,
            current,
            stale: false,
            recent: Recent::new(config.recent_keys),
            snapshots,
            changes: changes.clone(),
            config,
        };
        let task = tokio::spawn(worker.run(requests, stopped));
        Ok(Self {
            handle: ActorHandle {
                mailbox,
                snapshots: observed,
                changes,
            },
            stop: Some(stop),
            task: Some(task),
        })
    }

    pub fn handle(&self) -> ActorHandle {
        self.handle.clone()
    }

    /// Finish the command in progress, refuse queued ones as closed, and wait
    /// for the task to exit.
    pub async fn shutdown(mut self) -> Result<(), SpawnError> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        match self.task.take() {
            Some(task) => task.await.map_err(|_| SpawnError::Worker),
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

struct Worker {
    writer: Writer,
    readers: ReadPool,
    current: RuntimeSnapshot,
    /// Set when the store may hold a transition this actor did not observe.
    stale: bool,
    recent: Recent,
    snapshots: watch::Sender<RuntimeSnapshot>,
    changes: broadcast::Sender<StateChange>,
    config: ActorConfig,
}

impl Worker {
    async fn run(mut self, mut requests: mpsc::Receiver<Request>, mut stop: oneshot::Receiver<()>) {
        loop {
            let request = tokio::select! {
                biased;
                _ = &mut stop => break,
                request = requests.recv() => match request {
                    Some(request) => request,
                    None => return,
                },
            };
            let outcome = self.submit(request.key, request.event).await;
            let _ = request.reply.send(outcome);
        }
        requests.close();
        while let Some(request) = requests.recv().await {
            let _ = request.reply.send(Err(SubmitError::Closed));
        }
    }

    async fn submit(&mut self, key: SubmitKey, event: Event) -> Result<Applied, SubmitError> {
        if let Some((recorded, applied)) = self.recent.get(&key) {
            return if *recorded == event {
                Ok(Applied {
                    duplicate: true,
                    ..*applied
                })
            } else {
                Err(SubmitError::KeyConflict)
            };
        }
        if self.stale {
            self.reload().await?;
        }
        let to = match event.apply(self.current.state) {
            Ok(to) => to,
            // A retry of a transition committed before a restart is refused
            // by the state it produced; the durable record still answers it.
            Err(refused) => return self.replay(key, event, Some(refused)).await,
        };
        let next = self
            .current
            .state_seq
            .next()
            .ok_or(SubmitError::SequenceExhausted)?;
        let command = store::transition(&self.current, &key, event, to, next)?;
        let deadline = Instant::now() + self.config.write_timeout;
        match self.writer.write(command, deadline).await {
            Ok(_) => Ok(self.commit(key, event, to, next)),
            Err(WriteError::KeyConflict) => self.replay(key, event, None).await,
            Err(error @ (WriteError::OutcomeUnknown { .. } | WriteError::RowCount { .. })) => {
                self.stale = true;
                Err(error.into())
            }
            Err(error) => Err(error.into()),
        }
    }

    fn commit(
        &mut self,
        key: SubmitKey,
        event: Event,
        to: SessionRuntimeState,
        next: StateSeq,
    ) -> Applied {
        let from = self.current.state;
        self.current.state = to;
        self.current.state_seq = next;
        self.snapshots.send_replace(self.current.clone());
        let _ = self.changes.send(StateChange {
            session: self.current.session.clone(),
            event,
            from,
            to,
            state_seq: next,
        });
        let applied = Applied {
            state: to,
            state_seq: next,
            duplicate: false,
        };
        self.recent.insert(key, event, applied);
        applied
    }

    async fn replay(
        &mut self,
        key: SubmitKey,
        event: Event,
        refused: Option<crate::state::InvalidTransition>,
    ) -> Result<Applied, SubmitError> {
        let deadline = Instant::now() + self.config.read_timeout;
        match store::recorded(&self.readers, &self.current.session, &key, deadline).await? {
            Some(recorded) if recorded.event == event.as_str() => {
                let applied = Applied {
                    state: recorded.state,
                    state_seq: recorded.state_seq,
                    duplicate: false,
                };
                self.recent.insert(key, event, applied);
                Ok(Applied {
                    duplicate: true,
                    ..applied
                })
            }
            Some(_) => Err(SubmitError::KeyConflict),
            None => Err(refused.map_or(SubmitError::KeyConflict, SubmitError::from)),
        }
    }

    async fn reload(&mut self) -> Result<(), SubmitError> {
        let deadline = Instant::now() + self.config.read_timeout;
        let loaded = store::load(&self.readers, self.current.session.clone(), deadline).await?;
        self.stale = false;
        if loaded != self.current {
            self.current = loaded;
            self.snapshots.send_replace(self.current.clone());
        }
        Ok(())
    }
}
