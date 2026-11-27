//! Per-session coordinators survive both service selection and peer progress.
use super::*;
use tokio::sync::futures::OwnedNotified;

type SessionWork<'a> = Pin<Box<dyn Future<Output = DriveState> + Send + 'a>>;

pub(super) enum DriveState {
    Idle,
    Advanced,
}

struct SessionDrive<'a> {
    revision: Arc<()>,
    work: Option<SessionWork<'a>>,
    restart: bool,
}

/// Bounded by the connection registry. Futures belong to the service, not an
/// iteration of its select; only a binding retirement or shutdown drops them.
pub(in crate::dogfood) struct MailDrive<'a> {
    native: &'a NativeMailOwners,
    sessions: BTreeMap<SessionId, SessionDrive<'a>>,
    changed: Pin<Box<OwnedNotified>>,
}

impl<'a> MailDrive<'a> {
    pub(in crate::dogfood) fn new(native: &'a NativeMailOwners) -> Self {
        Self {
            native,
            sessions: BTreeMap::new(),
            changed: Box::pin(native.changed.clone().notified_owned()),
        }
    }

    pub(in crate::dogfood) async fn advance(&mut self) -> Result<bool, String> {
        if self.native.external_host {
            return pending().await;
        }
        poll_fn(|cx| {
            let notified = self.changed.as_mut().poll(cx).is_ready();
            if notified {
                self.changed = Box::pin(self.native.changed.clone().notified_owned());
            }
            let owners = match self.native.snapshot() {
                Ok(owners) => owners,
                Err(reason) => return Poll::Ready(Err(reason)),
            };
            self.sessions
                .retain(|session, _| owners.iter().any(|(id, _)| id == session));
            let mut completed = false;
            let mut advanced = false;
            for (session, owner) in owners {
                let revision = match owner.revision() {
                    Ok(revision) => revision,
                    Err(reason) => return Poll::Ready(Err(reason)),
                };
                let held = self
                    .sessions
                    .entry(session.clone())
                    .or_insert_with(|| SessionDrive {
                        revision: revision.clone(),
                        work: None,
                        restart: true,
                    });
                if !Arc::ptr_eq(&held.revision, &revision) {
                    // The old authenticated binding is retired, not a peer's work.
                    held.work = None;
                    held.revision = revision;
                    held.restart = true;
                }
                if held.work.is_none() && (held.restart || notified) {
                    held.work = Some(Box::pin(self.native.drive_one(session, owner)));
                    held.restart = false;
                }
                if let Some(work) = &mut held.work {
                    if let Poll::Ready(state) = work.as_mut().poll(cx) {
                        held.work = None;
                        held.restart = matches!(state, DriveState::Advanced);
                        advanced |= held.restart;
                        completed = true;
                    }
                }
                // Poll every session before yielding; an earlier peer cannot
                // starve an operation whose deadline is already running.
            }
            if completed || notified {
                Poll::Ready(Ok(advanced))
            } else {
                Poll::Pending
            }
        })
        .await
    }
}

pub(super) struct Observation {
    pub(super) live: bool,
    pub(super) restored: bool,
}

impl Owner {
    /// The transport operation lives in NativeMailContext::advance. Poll it
    /// without retaining its lease across Pending, so prompt preparation can
    /// read the last settled snapshot while this coordinator retains its retry.
    pub(super) async fn converge(
        &self,
        context: OwnedMutexGuard<NativeMailContext>,
        revision: &Arc<()>,
    ) -> (
        OwnedMutexGuard<NativeMailContext>,
        Option<Result<Observation, E>>,
    ) {
        let mut initial = Some(context);
        let mut lock = Box::pin(self.context.clone().lock_owned());
        poll_fn(|cx| {
            let mut context = match initial.take() {
                Some(context) => context,
                None => match lock.as_mut().poll(cx) {
                    Poll::Ready(context) => {
                        lock = Box::pin(self.context.clone().lock_owned());
                        context
                    }
                    Poll::Pending => return Poll::Pending,
                },
            };
            // Every poll follows a lease release. A prompt may have closed the
            // runtime or fenced this revision while the HTTP future was held.
            if self.planning.load(Ordering::SeqCst) != 0
                || self.superseded.load(Ordering::SeqCst)
                || !self
                    .revision()
                    .is_ok_and(|current| Arc::ptr_eq(&current, revision))
                || context.close_pending
                || context.requires_rebind()
                || context
                    .blocked_revision
                    .as_ref()
                    .is_some_and(|blocked| Arc::ptr_eq(blocked, revision))
            {
                return Poll::Ready((context, None));
            }
            let was_open = context.subscription_open;
            let was_live = context.has_live_runtime();
            let result = {
                let future = context.converge();
                tokio::pin!(future);
                future.as_mut().poll(cx)
            };
            match result {
                Poll::Ready(result) => Poll::Ready((
                    context,
                    Some(result.map(|live| Observation {
                        live,
                        restored: live && (!was_open || !was_live),
                    })),
                )),
                Poll::Pending => Poll::Pending,
            }
        })
        .await
    }
}
