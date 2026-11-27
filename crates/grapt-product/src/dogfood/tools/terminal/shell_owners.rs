//! Bounded session ownership for one-shot shell and configured-command cleanup.
use super::super::effects::{ShellCleanup, ShellControl, ShellOutcome};
use super::*;
use grapt_types::InterruptionSource;
use tokio::sync::watch;
use tokio::task::JoinHandle;

// Reap/census (4s), sandbox settlement (4s), output drain (2s), scheduling slack.
const SHELL_STOP_BOUND: Duration = Duration::from_secs(12);
type Key = (SessionId, OwnerId);

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct HelperOwnerId(u64);

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum OwnerId {
    Tool(ToolCallId),
    Helper(HelperOwnerId),
}

#[derive(Default)]
struct Registered {
    entries: BTreeMap<Key, Arc<Owner>>,
    next_helper: u64,
}

#[derive(Default)]
pub(super) struct ShellOwners {
    registered: Mutex<Registered>,
}

struct Owner {
    cancel: watch::Sender<Option<InterruptionSource>>,
    completion: watch::Receiver<Option<Result<(), String>>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Owner {
    fn cancel(&self, source: InterruptionSource) {
        self.cancel.send_if_modified(|current| {
            if current.is_some() {
                return false;
            }
            *current = Some(source);
            true
        });
    }

    fn evictable(&self) -> bool {
        matches!(&*self.completion.borrow(), Some(Ok(())))
            && lock(&self.task)
                .as_ref()
                .is_some_and(JoinHandle::is_finished)
    }

    async fn join(&self, deadline: tokio::time::Instant) -> Result<(), String> {
        let mut completion = self.completion.clone();
        let cleanup = tokio::time::timeout_at(deadline, async {
            loop {
                if let Some(result) = completion.borrow().clone() {
                    return result;
                }
                completion.changed().await.map_err(|_| {
                    "command owner ended without a cleanup receipt; cleanup is unconfirmed"
                        .to_owned()
                })?;
            }
        })
        .await
        .map_err(|_| "command cleanup did not settle within the stop bound".to_owned())?;
        // Retain the handle across cancellable awaits, including concurrent stops.
        let task = loop {
            {
                let mut task = lock(&self.task);
                if task.as_ref().is_none_or(JoinHandle::is_finished) {
                    break task.take();
                }
            }
            tokio::time::timeout_at(deadline, tokio::time::sleep(Duration::from_millis(10)))
                .await
                .map_err(|_| {
                    "command cleanup owner did not finish within the stop bound".to_owned()
                })?;
        };
        if let Some(task) = task {
            task.await
                .map_err(|error| format!("command cleanup owner failed: {error}"))?;
        }
        cleanup
    }
}

/// Dropping only the caller cancels; the registered owner is never aborted.
struct Caller(Arc<Owner>);

impl Drop for Caller {
    fn drop(&mut self) {
        self.0.cancel(InterruptionSource::User);
    }
}

impl ShellOwners {
    pub(super) fn cancel_session(&self, session: &SessionId) {
        for ((owner_session, _), owner) in &lock(&self.registered).entries {
            if owner_session == session {
                owner.cancel(InterruptionSource::ManagedShutdown);
            }
        }
    }

    pub(super) async fn stop(&self, session: &SessionId) -> Result<(), String> {
        let owners: Vec<_> = lock(&self.registered)
            .entries
            .iter()
            .filter(|((owner_session, _), _)| owner_session == session)
            .map(|(_, owner)| Arc::clone(owner))
            .collect();
        for owner in &owners {
            owner.cancel(InterruptionSource::ManagedShutdown);
        }
        let deadline = tokio::time::Instant::now() + SHELL_STOP_BOUND;
        let mut failures = Vec::new();
        for owner in owners {
            match owner.join(deadline).await {
                Ok(()) => {
                    lock(&self.registered)
                        .entries
                        .retain(|_, current| !Arc::ptr_eq(current, &owner));
                }
                Err(reason) => failures.push(reason),
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "one-shot command cleanup incomplete: {}",
                failures.join("; ")
            ))
        }
    }

    async fn run<T, F, Fut>(
        &self,
        session: SessionId,
        call: Option<ToolCallId>,
        observed_ms: Option<u64>,
        mut cancellation: Option<ToolCancellation>,
        work: F,
    ) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(ShellControl) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, String>> + Send + 'static,
    {
        let (reply, mut result) = oneshot::channel();
        let (cancel, owner_cancellation) = ToolCancellation::channel();
        if let Some(source) = cancellation
            .as_ref()
            .and_then(ToolCancellation::cancelled_by)
        {
            let _previous = cancel.send_replace(Some(source));
        }
        let (completed, completion) = watch::channel(None);
        let owner = Arc::new(Owner {
            cancel,
            completion,
            task: Mutex::new(None),
        });
        {
            let mut registered = lock(&self.registered);
            registered.entries.retain(|_, owner| !owner.evictable());
            if registered.entries.len() >= GLOBAL_RESOURCES
                || registered
                    .entries
                    .keys()
                    .filter(|(owner_session, _)| owner_session == &session)
                    .count()
                    >= SESSION_RESOURCES
            {
                return Err("command resource limit reached; stop an existing command before retrying; no command ran".into());
            }
            let identity = match call {
                Some(call) => OwnerId::Tool(call),
                None => {
                    let next = registered
                        .next_helper
                        .checked_add(1)
                        .ok_or("command helper identity space exhausted; no command ran")?;
                    registered.next_helper = next;
                    OwnerId::Helper(HelperOwnerId(next))
                }
            };
            let key = (session, identity);
            if registered.entries.contains_key(&key) {
                return Err("command call already has an execution owner; no command ran".into());
            }
            registered.entries.insert(key, Arc::clone(&owner));
            let task = tokio::spawn(async move {
                let cleanup = ShellCleanup::default();
                let result = if owner_cancellation.cancelled_by().is_some() {
                    Err("command cancelled before launch; no command ran".into())
                } else {
                    work(ShellControl {
                        observed_ms,
                        cancellation: owner_cancellation,
                        cleanup: cleanup.clone(),
                    })
                    .await
                };
                let settled = cleanup.result();
                let result = match (result, &settled) {
                    (Ok(value), Ok(())) => Ok(value),
                    (Ok(_), Err(cleanup)) => Err(format!("command cleanup unconfirmed: {cleanup}")),
                    (Err(reason), Ok(())) => Err(reason),
                    (Err(reason), Err(cleanup)) => {
                        Err(format!("{reason}; command cleanup unconfirmed: {cleanup}"))
                    }
                };
                // Publish completion only after all owner work and output delivery.
                drop(reply.send(result));
                let _previous = completed.send_replace(Some(settled));
            });
            *lock(&owner.task) = Some(task);
        }
        let caller = Caller(owner);
        let external_cancel = async {
            match &mut cancellation {
                Some(cancellation) => cancellation.changed().await,
                None => std::future::pending().await,
            }
        };
        let result = tokio::select! {
            result = &mut result => result,
            source = external_cancel => {
                caller.0.cancel(source.unwrap_or(InterruptionSource::ManagedShutdown));
                result.await
            }
        }
        .map_err(|_| "command owner ended without a result; cleanup is unconfirmed".to_owned())?;
        drop(caller);
        // Failed cleanup retains its capacity and witness until session stop.
        lock(&self.registered)
            .entries
            .retain(|_, owner| !owner.evictable());
        result
    }
}

impl TerminalRegistry {
    pub(in crate::dogfood::tools) async fn run_shell_owned<F, Fut>(
        &self,
        session: SessionId,
        call: ToolCallId,
        observed_ms: Option<u64>,
        cancellation: ToolCancellation,
        work: F,
    ) -> Result<ShellOutcome, String>
    where
        F: FnOnce(ShellControl) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<ShellOutcome, String>> + Send + 'static,
    {
        self.shells
            .run(
                session,
                Some(call),
                observed_ms,
                Some(cancellation),
                move |control| async move {
                    let cleanup = control.cleanup.clone();
                    let outcome = work(control).await?;
                    if cleanup.result().is_err() {
                        return Err(format!(
                            "shell captured output: {}",
                            String::from_utf8_lossy(&outcome.model_output()?)
                        ));
                    }
                    Ok(outcome)
                },
            )
            .await
    }

    pub(in crate::dogfood) async fn run_helper_owned<T, F, Fut>(
        &self,
        session: SessionId,
        work: F,
    ) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(ShellControl) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, String>> + Send + 'static,
    {
        self.shells.run(session, None, None, None, work).await
    }
}
