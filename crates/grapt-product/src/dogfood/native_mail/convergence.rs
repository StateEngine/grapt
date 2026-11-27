//! Retain one runtime slice across unrelated service-select cancellation.
use super::*;
use grapt_runtime::mailbox::remote::RemoteMailboxSlice;
use grapt_types::InterruptionSource;
use std::{future::Future, pin::Pin};
use tokio::sync::watch;

type Advanced = (HttpMailbox, Result<RemoteMailboxSlice, RemoteMailboxError>);

pub(super) struct Advance {
    work: Pin<Box<dyn Future<Output = Advanced> + Send>>,
    cancel: watch::Sender<Option<InterruptionSource>>,
    started_live: bool,
}

impl Advance {
    fn new(mut runtime: HttpMailbox) -> Self {
        let started_live = runtime.is_live();
        let (cancel, signal) = watch::channel(None);
        let work = Box::pin(async move {
            let result = runtime.advance_slice(&signal).await;
            (runtime, result)
        });
        Self {
            work,
            cancel,
            started_live,
        }
    }
}

impl NativeMailContext {
    /// Moving an already-live runtime into its bounded delivery operation does
    /// not close the snapshot. Completion restores it before releasing the lease.
    pub(super) fn has_live_runtime(&self) -> bool {
        self.runtime.as_ref().is_some_and(HttpMailbox::is_live)
            || self.advance.as_ref().is_some_and(|work| work.started_live)
    }

    pub(super) fn owns_runtime(&self) -> bool {
        self.runtime.is_some() || self.advance.is_some()
    }

    pub(super) async fn advance_slice(&mut self) -> Result<RemoteMailboxSlice, RemoteMailboxError> {
        if self.advance.is_none() {
            let runtime = self.runtime.take().ok_or(RemoteMailboxError::NotLive)?;
            self.advance = Some(Advance::new(runtime));
        }
        self.finish_advance().await
    }

    async fn finish_advance(&mut self) -> Result<RemoteMailboxSlice, RemoteMailboxError> {
        let pending = self.advance.as_mut().ok_or(RemoteMailboxError::NotLive)?;
        let (runtime, result) = pending.work.as_mut().await;
        self.runtime = Some(runtime);
        self.advance = None;
        result
    }

    pub(super) async fn stop_advance(&mut self) -> Result<(), RemoteMailboxError> {
        if let Some(pending) = &self.advance {
            // The runtime observes this at each bounded port operation. Join
            // that cancellation before closing the same retained binding.
            let _previous = pending
                .cancel
                .send_replace(Some(InterruptionSource::ManagedShutdown));
            match self.finish_advance().await {
                Ok(_) | Err(RemoteMailboxError::Cancelled) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}
