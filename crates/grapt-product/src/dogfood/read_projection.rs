//! Run a whole durable projection on one private snapshot off async workers.
use super::{DogfoodApplication, Inbox};
use grapt_api::u3a::ApplicationFuture;
use grapt_store::read_pool::ReadPool;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;

// Bounds blocking dispatch and reader checkout. Once acquired, a snapshot's
// bounded rows and bytes may take longer to hydrate and encode.
const READ_WAIT_DEADLINE: Duration = Duration::from_millis(500);

#[derive(Default)]
struct Cancellation {
    cancelled: bool,
    reading: bool,
    interrupt: Option<rusqlite::InterruptHandle>,
}

struct CancelOnDrop {
    state: Arc<Mutex<Cancellation>>,
    task: tokio::task::AbortHandle,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        // Aborting removes work that has not started. Active work gets an
        // SQLite interrupt and checks cancellation at the lease boundaries.
        self.task.abort();
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.cancelled = true;
        if let Some(interrupt) = &state.interrupt {
            interrupt.interrupt();
        }
    }
}

struct ActiveRead(Arc<Mutex<Cancellation>>);

impl Drop for ActiveRead {
    fn drop(&mut self) {
        // Detach before recycling: a late caller cancellation must never
        // interrupt the next owner of this connection.
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .interrupt = None;
    }
}

fn check_active(state: &Mutex<Cancellation>, deadline: Instant) -> Result<(), String> {
    let state = state
        .lock()
        .map_err(|_| "read cancellation mutex is poisoned")?;
    if state.cancelled || (!state.reading && Instant::now() >= deadline) {
        Err(ReadRefusal::Busy.detail().to_owned())
    } else {
        Ok(())
    }
}

fn begin_read(state: &Mutex<Cancellation>, deadline: Instant) -> Result<(), String> {
    let mut state = state
        .lock()
        .map_err(|_| "read cancellation mutex is poisoned")?;
    if state.cancelled || Instant::now() >= deadline {
        return Err(ReadRefusal::Busy.detail().to_owned());
    }
    // Share the transition with the waiting caller: once this wins the race,
    // the deadline cannot interrupt the acquired snapshot's projection.
    state.reading = true;
    Ok(())
}

pub(crate) fn blocking<T>(work: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

/// Why a read could not run: a busy reader, or something that will not recover.
///
/// A capacity wait is its own variant rather than prose in a `String`, because the
/// route answers it with 503 and `Retry-After` and a caller must be able to tell it
/// from a bad request without matching a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadRefusal {
    /// The reader was busy for the whole wait. Transient: retry.
    Busy,
    /// Anything else: an unreadable store, a poisoned lock, a replaced database.
    Failed,
}

impl ReadRefusal {
    /// The sentence a caller turns into its own refusal type.
    ///
    /// Kept as a method so the text lives beside the classification, and so the
    /// two can never disagree about which refusals are retryable.
    pub(crate) fn detail(self) -> &'static str {
        match self {
            // The user-visible wording callers already route on; the change is
            // that its retryable meaning is now carried by the variant.
            ReadRefusal::Busy => "history read deadline expired; retry the request",
            ReadRefusal::Failed => "history read is unavailable",
        }
    }
}

impl From<grapt_store::read_pool::ReadPoolError> for ReadRefusal {
    fn from(error: grapt_store::read_pool::ReadPoolError) -> Self {
        match error {
            grapt_store::read_pool::ReadPoolError::Busy => ReadRefusal::Busy,
            _ => ReadRefusal::Failed,
        }
    }
}

/// An application read waits for its reader no longer than its own deadline.
fn inbox_by(pool: &ReadPool, deadline: Instant) -> Result<Arc<Mutex<Inbox>>, ReadRefusal> {
    pool.checkout_within(|| deadline.saturating_duration_since(Instant::now()))
        .map(|read| Arc::new(Mutex::new(Inbox::from_read(read))))
        .map_err(ReadRefusal::from)
}

pub(crate) fn inbox_typed(
    pool: &ReadPool,
) -> Result<Arc<Mutex<Inbox>>, grapt_store::read_pool::ReadPoolError> {
    pool.checkout()
        .map(|read| Arc::new(Mutex::new(Inbox::from_read(read))))
}

pub(crate) fn finish(inbox: Arc<Mutex<Inbox>>) -> Result<(), String> {
    finish_typed(inbox).map_err(|error| match error {
        Finish::Pool(error) => error.to_string(),
        Finish::Retained(detail) => detail.to_owned(),
    })
}

/// Why ending a finite read failed: the pool's own error, kept typed so a
/// busy reader stays a capacity wait, or a projection that kept its snapshot.
pub(crate) enum Finish {
    Pool(grapt_store::read_pool::ReadPoolError),
    Retained(&'static str),
}

pub(crate) fn finish_typed(inbox: Arc<Mutex<Inbox>>) -> Result<(), Finish> {
    let inbox = Arc::try_unwrap(inbox)
        .map_err(|_| Finish::Retained("read projection retained its snapshot"))?;
    inbox
        .into_inner()
        .map_err(|_| Finish::Retained("read projection mutex is poisoned"))?
        .finish_read()
        .map_err(Finish::Pool)
}

/// End a read that did not produce a value without returning its connection.
///
/// The pool cannot tell an I/O failure from a domain refusal: `project` returns
/// a caller-chosen error type, and no read carries SQLite's code out of it. So
/// the rule is drawn on what is knowable — the projection did not complete —
/// and the cost of being conservative is one connection open on a path that
/// already failed. Recycling instead would risk handing an unrelated read a
/// reader that cannot reach the database, which is the failure this prevents.
/// A successful read still recycles through [`finish_typed`].
pub(crate) fn discard(inbox: Arc<Mutex<Inbox>>) {
    if let Ok(inbox) = Arc::try_unwrap(inbox) {
        if let Ok(inbox) = inbox.into_inner() {
            inbox.discard_read();
        }
    }
}

impl DogfoodApplication {
    pub(crate) fn read_projection<T: Send + 'static, E: Send + 'static>(
        &self,
        project: impl FnOnce(&Self) -> Result<T, E> + Send + 'static,
        failed: fn(String) -> E,
    ) -> ApplicationFuture<T, E> {
        let application = self.clone();
        let deadline = Instant::now() + READ_WAIT_DEADLINE;
        Box::pin(async move {
            let state = Arc::new(Mutex::new(Cancellation::default()));
            let worker = Arc::clone(&state);
            let mut task = tokio::task::spawn_blocking(move || {
                check_active(&worker, deadline).map_err(failed)?;
                let mut application = application;
                let Some(pool) = application.readers.take() else {
                    begin_read(&worker, deadline).map_err(failed)?;
                    return project(&application);
                };
                application.inbox = inbox_by(&pool, deadline)
                    .map_err(|refusal| failed(refusal.detail().to_owned()))?;
                let interrupt = application
                    .inbox
                    .lock()
                    .map_err(|_| failed("read projection mutex is poisoned".into()))?
                    .store()
                    .connection()
                    .get_interrupt_handle();
                worker
                    .lock()
                    .map_err(|_| failed("read cancellation mutex is poisoned".into()))?
                    .interrupt = Some(interrupt);
                let active = ActiveRead(Arc::clone(&worker));
                begin_read(&worker, deadline).map_err(failed)?;
                let result = project(&application);
                let completed = check_active(&worker, deadline);
                drop(active);
                // A read that produced no value closes its reader: the pool
                // cannot tell an I/O failure from a refusal, and only this read
                // knows the projection ended badly.
                if result.is_err() {
                    discard(application.inbox);
                } else {
                    finish(application.inbox).map_err(failed)?;
                }
                completed.map_err(failed)?;
                result
            });
            let _cancel = CancelOnDrop {
                state: Arc::clone(&state),
                task: task.abort_handle(),
            };
            let completed = match tokio::time::timeout_at(deadline, &mut task).await {
                Ok(completed) => completed,
                Err(_) => {
                    let reading = {
                        let mut state = state
                            .lock()
                            .map_err(|_| failed("read cancellation mutex is poisoned".into()))?;
                        if !state.reading {
                            state.cancelled = true;
                        }
                        state.reading
                    };
                    if !reading {
                        return Err(failed(ReadRefusal::Busy.detail().to_owned()));
                    }
                    task.await
                }
            };
            completed.map_err(|error| failed(format!("history read task failed: {error}")))?
        })
    }
}

impl DogfoodApplication {
    pub(crate) fn read_session(
        &self,
        session: grapt_types::SessionId,
    ) -> ApplicationFuture<
        grapt_api::u3a::SessionResponseDto,
        grapt_api::u3a::SessionLookupApplicationError,
    > {
        use grapt_api::u3a::SessionLookupApplicationError;
        self.read_projection(
            move |application| super::session_info::lookup(application, &session),
            |_| SessionLookupApplicationError::Unavailable,
        )
    }
}
