use crate::config::ActorConfig;
use crate::dedup::Recent;
use crate::error::{SpawnError, SubmitError};
use crate::key::SubmitKey;
use crate::state::{
    Applied, Event, InvalidTransition, RuntimeSnapshot, SessionRuntimeState, StateChange, StateSeq,
};
use crate::store;
use grapt_run_store::{ReadPool, WriteError, Writer};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::time::Instant;

pub(crate) struct Request {
    pub(crate) key: SubmitKey,
    pub(crate) event: Event,
    pub(crate) reply: oneshot::Sender<Result<Applied, SubmitError>>,
}

/// A transition built from the current snapshot. Rebuilding it from the same
/// snapshot yields the same command, so resending it under its key settles
/// an earlier attempt whose outcome is unknown.
struct Transition {
    key: SubmitKey,
    event: Event,
    to: SessionRuntimeState,
    next: StateSeq,
}

/// The serialized owner of one session's state.
pub(crate) struct Worker {
    writer: Writer,
    readers: ReadPool,
    current: RuntimeSnapshot,
    /// Another writer may have changed the session since `current` was read.
    stale: bool,
    /// A write the store admitted whose outcome this worker has not seen.
    /// Nothing else is written, read or answered from memory until it settles.
    pending: Option<Transition>,
    recent: Recent,
    snapshots: watch::Sender<RuntimeSnapshot>,
    changes: broadcast::Sender<StateChange>,
    config: ActorConfig,
}

impl Worker {
    pub(crate) fn new(
        writer: Writer,
        readers: ReadPool,
        current: RuntimeSnapshot,
        snapshots: watch::Sender<RuntimeSnapshot>,
        changes: broadcast::Sender<StateChange>,
        config: ActorConfig,
    ) -> Self {
        Self {
            writer,
            readers,
            current,
            stale: false,
            pending: None,
            recent: Recent::new(config.recent_keys),
            snapshots,
            changes,
            config,
        }
    }

    pub(crate) async fn run(
        mut self,
        mut requests: mpsc::Receiver<Request>,
        mut stop: oneshot::Receiver<()>,
    ) -> Result<(), SpawnError> {
        loop {
            let request = tokio::select! {
                biased;
                _ = &mut stop => break,
                request = requests.recv() => match request {
                    Some(request) => request,
                    None => break,
                },
            };
            let outcome = self.submit(request.key, request.event).await;
            let _ = request.reply.send(outcome);
        }
        requests.close();
        while let Some(request) = requests.recv().await {
            let _ = request.reply.send(Err(SubmitError::Closed));
        }
        self.settle().await.map_err(|_| SpawnError::Unsettled)?;
        // A snapshot that cannot be reconciled is not a clean stop.
        if self.stale {
            self.reload().await.map_err(|_| SpawnError::Unsettled)?;
        }
        Ok(())
    }

    async fn submit(&mut self, key: SubmitKey, event: Event) -> Result<Applied, SubmitError> {
        self.settle()
            .await
            .map_err(|_| SubmitError::PriorUnsettled)?;
        // A retained reload failure is reported here, before any answer.
        if self.stale {
            self.reload().await?;
        }
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
        self.write(Transition {
            key,
            event,
            to,
            next,
        })
        .await
    }

    async fn write(&mut self, transition: Transition) -> Result<Applied, SubmitError> {
        let command = self.command(&transition)?;
        let deadline = Instant::now() + self.config.write_timeout;
        match self.writer.write(command, deadline).await {
            Ok(receipt) if !receipt.replayed => Ok(self.commit(transition)),
            // Another writer committed this exact transition first and
            // published it; only the snapshot catches up.
            Ok(_) => {
                self.resync().await?;
                let applied = Applied {
                    state: transition.to,
                    state_seq: transition.next,
                    duplicate: false,
                };
                self.recent
                    .insert(transition.key, transition.event, applied);
                Ok(Applied {
                    duplicate: true,
                    ..applied
                })
            }
            Err(WriteError::KeyConflict) => {
                self.replay(transition.key, transition.event, None).await
            }
            Err(WriteError::OutcomeUnknown { .. }) => {
                self.pending = Some(transition);
                Err(SubmitError::OutcomeUnknown)
            }
            // The compare-and-set found a sequence another writer moved.
            Err(WriteError::RowCount { .. }) => {
                self.resync().await?;
                Err(SubmitError::Contended)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Resend the pending transition under its key. The writer never reorders
    /// requests that share a key, so any answer it gives comes after the
    /// earlier attempt finished. A fresh receipt means this worker made the
    /// commit; otherwise only the keyed transition record can settle it.
    async fn settle(&mut self) -> Result<(), SubmitError> {
        let Some(transition) = self.pending.take() else {
            return Ok(());
        };
        let outcome = match self.command(&transition) {
            Ok(command) => {
                let deadline = Instant::now() + self.config.write_timeout;
                self.writer.write(command, deadline).await
            }
            Err(error) => Err(error),
        };
        match settlement(&outcome) {
            Settlement::Committed => {
                self.commit(transition);
                Ok(())
            }
            Settlement::NotCommitted => self.resync().await,
            Settlement::Unresolved => {
                self.pending = Some(transition);
                Err(outcome.map_or_else(SubmitError::from, |_| SubmitError::OutcomeUnknown))
            }
            Settlement::Check => {
                let deadline = Instant::now() + self.config.read_timeout;
                let session = &self.current.session;
                match store::recorded(&self.readers, session, &transition.key, deadline).await {
                    // The key is recorded, so the earlier attempt, or another
                    // writer's identical one, committed; the snapshot catches up.
                    Ok(Some(_)) => self.resync().await,
                    Ok(None) => {
                        self.pending = Some(transition);
                        Err(outcome.map_or_else(SubmitError::from, |_| SubmitError::OutcomeUnknown))
                    }
                    Err(error) => {
                        self.pending = Some(transition);
                        Err(error.into())
                    }
                }
            }
        }
    }

    fn command(&self, transition: &Transition) -> Result<grapt_run_store::Command, WriteError> {
        store::transition(
            &self.current,
            &transition.key,
            transition.event,
            transition.to,
            transition.next,
        )
    }

    fn commit(&mut self, transition: Transition) -> Applied {
        let Transition {
            key,
            event,
            to,
            next,
        } = transition;
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
        refused: Option<InvalidTransition>,
    ) -> Result<Applied, SubmitError> {
        let deadline = Instant::now() + self.config.read_timeout;
        match store::recorded(&self.readers, &self.current.session, &key, deadline).await? {
            Some(recorded) if recorded.event == event.as_str() => {
                if recorded.state_seq > self.current.state_seq {
                    self.resync().await?;
                }
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

    /// Catch the snapshot up with the store before answering. A failed read
    /// fails the answer and stays recorded as `stale` until a reload succeeds.
    async fn resync(&mut self) -> Result<(), SubmitError> {
        self.stale = true;
        self.reload().await
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

/// What a resend's answer says about the pending transition.
#[derive(Debug, Eq, PartialEq)]
enum Settlement {
    /// This resend made the commit.
    Committed,
    /// The writer found the key absent and refused the effects, so the
    /// transition did not commit.
    NotCommitted,
    /// The resend never reached the writer; the earlier attempt may still run.
    Unresolved,
    /// The earlier attempt is finished, but this answer does not say whether
    /// its key committed: a replayed receipt, or a refusal reached before the
    /// receipt was checked.
    Check,
}

fn settlement(outcome: &Result<grapt_run_store::Receipt, WriteError>) -> Settlement {
    match outcome {
        Ok(receipt) if !receipt.replayed => Settlement::Committed,
        Err(WriteError::RowCount { .. }) => Settlement::NotCommitted,
        Err(
            WriteError::QueueFull
            | WriteError::ByteBudget
            | WriteError::WalPressure
            | WriteError::NotAdmitted
            | WriteError::Closed
            | WriteError::OutcomeUnknown { .. },
        ) => Settlement::Unresolved,
        _ => Settlement::Check,
    }
}
