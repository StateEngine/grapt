//! Connection custody in the existing service driver, separate from prompt lifetime.
//! One bounded runtime per Session; no task, inbox, durable writer or status map.
use super::*;
use grapt_api::{
    u3a::{DogfoodEvent, FiniteF64, LiveEvent, MailAttentionData, WireId},
    DogfoodEventHub,
};
use grapt_runtime::mailbox::remote::http::{HttpMailboxChange, HttpMailboxChangeResult};
use grapt_types::{ClockAuthority, EventId, MessageId};
use std::{
    collections::BTreeMap,
    future::{pending, poll_fn, Future},
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::Poll,
};
use tokio::sync::{Mutex as AsyncMutex, Notify, OwnedMutexGuard};

const OWNER_CAP: usize = 64;
mod driving;
mod probe;
use driving::DriveState;
pub(in crate::dogfood) use driving::MailDrive;
mod wake;
pub(in crate::dogfood) use wake::{NativeWakeFence, NativeWakeRefusal};
type CloseWork = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

type OwnedWork<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

#[derive(Clone)]
struct AttentionSnapshot {
    revision: Arc<()>,
    status: MailAttentionInfo,
    observed: i64,
}

pub(in crate::dogfood) struct NativeMailOwners {
    external_host: bool,
    inbox: Arc<Mutex<Inbox>>,
    custody: Arc<Mutex<SessionCapabilities>>,
    ids: Arc<dyn IdAuthority + Send + Sync>,
    clock: Arc<dyn ClockAuthority + Send + Sync>,
    hub: Arc<DogfoodEventHub>,
    service_root: std::path::PathBuf,
    owners: Mutex<BTreeMap<SessionId, Arc<Owner>>>,
    changed: Arc<Notify>,
    wake_application: std::sync::OnceLock<std::sync::Weak<super::super::DogfoodApplication>>,
    wake_cursor: Mutex<Option<SessionId>>,
    wake_capacity_waiting: AtomicBool,
    retirement_cleanup_failed: AtomicBool,
}
pub(in crate::dogfood) struct Owner {
    location: grapt_types::LocationRef,
    context: Arc<AsyncMutex<NativeMailContext>>,
    // Presentation only, fenced by the same revision as the retained context.
    attention_cache: Mutex<Option<AttentionSnapshot>>,
    planning: AtomicUsize,
    target_scope: Mutex<Option<RemoteMailboxScope>>,
    target_registered: AtomicBool,
    superseded: AtomicBool,
    wake_scheduled: AtomicBool,
    wake_blocked: Mutex<Option<BlockedNativeWake>>,
    revision: Mutex<Arc<()>>,
    wait: Mutex<Option<MailWait>>,
    recovery: Mutex<super::recovery::Recovery>,
    changed: Arc<Notify>,
}

struct BlockedNativeWake {
    input: MessageId,
    revision: Arc<()>,
    retry_after_connection: bool,
}

// Retain both the operation and its result across cancellation of the combined
// service select. A received advisory stays here until runtime custody is free.
enum MailWait {
    Waiting(HttpMailboxChange),
    Received(HttpMailboxChangeResult),
}

/// A guard requests priority before waiting for the runtime lock. Its drop
/// wakes the service even if the prompt task panics or is forcibly aborted.
struct Planning(Arc<Owner>);
impl Drop for Planning {
    fn drop(&mut self) {
        self.0.planning.fetch_sub(1, Ordering::SeqCst);
        self.0.changed.notify_one();
    }
}
pub(in crate::dogfood) struct NativeMailPlan {
    external_host: bool,
    owner: Arc<Owner>,
    lease: Option<OwnedMutexGuard<NativeMailContext>>,
    planning: Option<Planning>,
    service_owned: bool,
    /// The outage this step's preparation degraded on; mail stays pending.
    degraded: Option<E>,
    /// A degraded preparation still closes as a cancelled one would.
    cleanup_owed: bool,
}

impl Owner {
    fn cache_attention(&self, context: &NativeMailContext) {
        if let Ok(mut cache) = self.attention_cache.lock() {
            *cache = context
                .verified_revision
                .as_ref()
                .map(|revision| AttentionSnapshot {
                    revision: revision.clone(),
                    status: context.attention,
                    observed: context.clock.now().as_millis(),
                });
        }
    }
    fn connection_restored(&self) -> Result<(), String> {
        let mut blocked = self
            .wake_blocked
            .lock()
            .map_err(|_| "native wake execution custody unavailable")?;
        if blocked
            .as_ref()
            .is_some_and(|blocked| blocked.retry_after_connection)
        {
            *blocked = None;
        }
        Ok(())
    }

    fn new(context: NativeMailContext, changed: Arc<Notify>) -> Arc<Self> {
        Arc::new(Self {
            location: context.location.clone(),
            context: Arc::new(AsyncMutex::new(context)),
            attention_cache: Mutex::new(None),
            planning: AtomicUsize::new(0),
            target_scope: Mutex::new(None),
            target_registered: AtomicBool::new(false),
            superseded: AtomicBool::new(false),
            wake_scheduled: AtomicBool::new(false),
            wake_blocked: Mutex::new(None),
            revision: Mutex::new(Arc::new(())),
            wait: Mutex::new(None),
            recovery: Mutex::new(super::recovery::Recovery::default()),
            changed,
        })
    }
    fn revision(&self) -> Result<Arc<()>, String> {
        self.revision
            .lock()
            .map(|v| v.clone())
            .map_err(|_| "native inbox revision custody is unavailable".into())
    }
    fn cancel_wait(&self) -> Result<(), String> {
        self.wait
            .lock()
            .map(|mut wait| {
                drop(wait.take());
            })
            .map_err(|_| "native inbox wait custody is unavailable".into())
    }
}

mod plan;
mod registry;
mod target;

impl NativeMailOwners {
    pub(in crate::dogfood) fn new(
        inbox: Arc<Mutex<Inbox>>,
        custody: Arc<Mutex<SessionCapabilities>>,
        ids: Arc<dyn IdAuthority + Send + Sync>,
        clock: Arc<dyn ClockAuthority + Send + Sync>,
        hub: Arc<DogfoodEventHub>,
        service_root: &std::path::Path,
    ) -> Arc<Self> {
        Self::with_external_host(inbox, custody, ids, clock, hub, service_root, false)
    }

    pub(in crate::dogfood) fn with_external_host(
        inbox: Arc<Mutex<Inbox>>,
        custody: Arc<Mutex<SessionCapabilities>>,
        ids: Arc<dyn IdAuthority + Send + Sync>,
        clock: Arc<dyn ClockAuthority + Send + Sync>,
        hub: Arc<DogfoodEventHub>,
        service_root: &std::path::Path,
        external_host: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            external_host,
            inbox,
            custody,
            ids,
            clock,
            hub,
            service_root: service_root.to_path_buf(),
            owners: Mutex::new(BTreeMap::new()),
            changed: Arc::new(Notify::new()),
            wake_application: std::sync::OnceLock::new(),
            wake_cursor: Mutex::new(None),
            wake_capacity_waiting: AtomicBool::new(false),
            retirement_cleanup_failed: AtomicBool::new(false),
        })
    }
    pub(in crate::dogfood) fn plan(&self, session: &SessionId) -> Result<NativeMailPlan, String> {
        // Anonymous sessions have no remote connection to retain and do not
        // spend native connection capacity. The same private owner closes them.
        let context = NativeMailContext::new(
            session.clone(),
            self.inbox.clone(),
            self.custody.clone(),
            self.ids.clone(),
            self.clock.clone(),
        )
        .map_err(|e| e.to_string())?;
        if self.external_host {
            // External notices are admitted by the independent host. Never
            // open a second mail transport or replace its selected target.
            let mut plan = NativeMailPlan::private(context);
            plan.external_host = true;
            return Ok(plan);
        }
        let binding = context.current_binding();
        let target_binding = binding
            .as_ref()
            .ok()
            .and_then(|b| b.as_ref())
            .map(|b| (b.scope.clone(), b.raw.clone(), b.capability.clone()));
        match binding {
            Ok(Some(_)) => {}
            // Defer an invalid binding to the established context preparation
            // owner, preserving its typed continuity refusal and recovery row.
            Ok(None) | Err(_) => return Ok(NativeMailPlan::private(context)),
        }
        let mut owners = self
            .owners
            .lock()
            .map_err(|_| "native inbox owners are unavailable")?;
        self.prune_retired(&mut owners);
        let (owner, inserted) = self.retain_context(
            &mut owners,
            session,
            context,
            target_binding.as_ref().map(|binding| &binding.0),
        )?;
        if !owner.target_registered.load(Ordering::SeqCst) {
            if let Some(binding) = target_binding {
                if let Err(error) = self.select_target(&owners, session, &owner, binding) {
                    if inserted {
                        owners.remove(session);
                    }
                    return Err(error);
                }
            }
        }
        if inserted
            && owners
                .values()
                .filter(|owner| !owner.superseded.load(Ordering::SeqCst))
                .count()
                > OWNER_CAP
        {
            owners.remove(session);
            return Err("Native inbox connection capacity changed; retry the binding.".into());
        }
        self.changed.notify_one();
        Ok(NativeMailPlan {
            external_host: false,
            owner,
            lease: None,
            planning: None,
            service_owned: true,
            degraded: None,
            cleanup_owed: false,
        })
    }
    pub(in crate::dogfood) fn invalidate(&self, session: &SessionId) {
        let owner = self
            .owners
            .lock()
            .ok()
            .and_then(|v| v.get(session).cloned());
        if let Some(owner) = owner {
            owner.target_registered.store(false, Ordering::SeqCst);
            let _cancelled = owner.cancel_wait();
            if let Ok(mut recovery) = owner.recovery.lock() {
                recovery.reset();
            }
            if let Ok(mut revision) = owner.revision.lock() {
                *revision = Arc::new(());
            }
        }
        self.changed.notify_one();
    }
    /// Re-register the sessions a stopped service was listening for. Owners
    /// live only in memory, so a restart is otherwise silently deaf.
    pub(in crate::dogfood) fn restore_selected(&self) {
        if self.external_host {
            return;
        }
        // The guard drops before `plan`, which reads the binding itself.
        let selected = self
            .inbox
            .lock()
            .map_err(|_| "native inbox is unavailable".to_owned())
            .and_then(|inbox| {
                inbox
                    .selected_native_mail_sessions(OWNER_CAP)
                    .map_err(|error| error.to_string())
            });
        let sessions = match selected {
            Ok(sessions) => sessions,
            Err(cause) => {
                crate::server_log::error(format_args!("native mail restore failed cause={cause}"));
                return;
            }
        };
        let waiting = sessions.iter().filter(|s| self.restore(s)).count();
        crate::server_log::info(format_args!(
            "native mail restart selected={} waiting for re-provision={waiting}",
            sessions.len()
        ));
    }
    /// The capability is process-private, so after a restart only the seat's
    /// next provision can supply it. Until then the owner parks visibly as
    /// RebindRequired; provision's `plan()` then sees a capability and opens.
    fn restore(&self, session: &SessionId) -> bool {
        let Ok(mut context) = NativeMailContext::new(
            session.clone(),
            self.inbox.clone(),
            self.custody.clone(),
            self.ids.clone(),
            self.clock.clone(),
        ) else {
            return false;
        };
        if !matches!(context.current_binding(), Err(E::Capability)) {
            // A capability already in custody takes the ordinary open path.
            let _planned = self.plan(session);
            return false;
        }
        context.require_rebind();
        let Ok(mut owners) = self.owners.lock() else {
            return false;
        };
        let retained = self.retain_context(&mut owners, session, context, None);
        drop(owners);
        if retained.is_err() {
            return false;
        }
        self.changed.notify_one();
        crate::server_log::info(format_args!(
            "native mail waiting for re-provision session={session}"
        ));
        true
    }
    fn snapshot(&self) -> Result<Vec<(SessionId, Arc<Owner>)>, String> {
        self.owners
            .lock()
            .map(|v| v.iter().map(|(s, o)| (s.clone(), o.clone())).collect())
            .map_err(|_| "native inbox owners are unavailable".into())
    }
    fn frame(
        &self,
        session: &SessionId,
        status: MailAttentionInfo,
        location: grapt_types::LocationRef,
        stale_since: Option<FiniteF64>,
    ) -> Option<DogfoodEvent> {
        let id = WireId::<EventId>::parse(format!("evt_{}", self.ids.mint().as_str()))?;
        let session = WireId::parse(session.as_str().to_owned())?;
        let created = FiniteF64::try_new(self.clock.now().as_millis() as f64).ok()?;
        Some(DogfoodEvent::MailAttention(LiveEvent::new(
            id,
            created,
            MailAttentionData::new(session, status, location).with_stale_since(stale_since),
        )))
    }
    fn publish(
        &self,
        session: &SessionId,
        status: MailAttentionInfo,
        location: grapt_types::LocationRef,
    ) {
        if let Some(frame) = self.frame(session, status, location, None) {
            if self.hub.publish(&frame).is_ok() {
                return;
            }
        }
        // Attention is shown across Sessions, so every stream reloads.
        self.hub
            .invalidate_because("native inbox attention could not be published");
    }
    /// Courtesy reconnect frames: read only the owner's derived projection,
    /// never the DB or credentials under the hub's subscriber lock.
    pub(in crate::dogfood) fn hydrate(&self) -> Vec<DogfoodEvent> {
        let Ok(owners) = self.snapshot() else {
            return Vec::new();
        };
        owners
            .into_iter()
            .take(32)
            .filter_map(|(session, owner)| {
                let revision = owner.revision().ok();
                let mut stale_since = None;
                let status = match owner.context.try_lock() {
                    Ok(context)
                        if revision.as_ref().is_some_and(|revision| {
                            context
                                .verified_revision
                                .as_ref()
                                .is_some_and(|checked| Arc::ptr_eq(checked, revision))
                        }) =>
                    {
                        context.attention
                    }
                    Ok(context)
                        if context.attention == MailAttentionInfo::Retrying
                            && context.blocked_revision.is_none() =>
                    {
                        MailAttentionInfo::Retrying
                    }
                    Ok(context) if context.slow_stopped => MailAttentionInfo::Stopped,
                    Ok(_) => MailAttentionInfo::Unavailable,
                    Err(_) => {
                        // Never wait for the StateEngine I/O lease. An old
                        // revision cannot disclose even its pending count.
                        let cached = owner.attention_cache.try_lock().ok().and_then(|cache| {
                            cache
                                .as_ref()
                                .filter(|snapshot| {
                                    revision.as_ref().is_some_and(|revision| {
                                        Arc::ptr_eq(&snapshot.revision, revision)
                                    })
                                })
                                .cloned()
                        });
                        match cached {
                            Some(snapshot) => {
                                stale_since = FiniteF64::try_new(snapshot.observed as f64).ok();
                                snapshot.status
                            }
                            None => {
                                stale_since =
                                    FiniteF64::try_new(self.clock.now().as_millis() as f64).ok();
                                MailAttentionInfo::Unavailable
                            }
                        }
                    }
                };
                self.frame(&session, status, owner.location.clone(), stale_since)
            })
            .collect()
    }
    #[cfg(test)]
    pub(in crate::dogfood) async fn drive(&self) -> Result<(), String> {
        let mut drive = MailDrive::new(self);
        while !drive.advance().await? {}
        Ok(())
    }
    #[expect(clippy::cognitive_complexity, reason = "legacy")]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    async fn drive_one(&self, session: SessionId, owner: Arc<Owner>) -> DriveState {
        if owner.planning.load(Ordering::SeqCst) != 0 {
            return DriveState::Idle;
        }
        if owner.retire_if_superseded().await {
            return DriveState::Idle;
        }
        let Ok(revision) = owner.revision() else {
            return DriveState::Idle;
        };
        // No context guard crosses the backoff. Prompt work has priority and
        // cancellation retains this owner's timer rather than restarting it.
        poll_fn(|cx| match owner.recovery.lock() {
            Ok(mut recovery) => recovery.poll(cx),
            Err(_) => Poll::Pending,
        })
        .await;
        let mut context = owner.context.clone().lock_owned().await;
        let mut coalesced = false;
        loop {
            if owner.planning.load(Ordering::SeqCst) != 0
                || !owner
                    .revision()
                    .is_ok_and(|current| Arc::ptr_eq(&current, &revision))
            {
                return DriveState::Idle;
            }
            if owner.superseded.load(Ordering::SeqCst) {
                if context.owns_runtime() {
                    context.close_pending = true;
                    let _cancelled = owner.cancel_wait();
                    self.changed.notify_one();
                }
                owner.cache_attention(&context);
                drop(context);
                return DriveState::Idle;
            }
            if context.close_pending {
                owner.cache_attention(&context);
                drop(context);
                return DriveState::Idle;
            }
            if context.requires_rebind() {
                let changed = context
                    .verified_revision
                    .as_ref()
                    .is_none_or(|checked| !Arc::ptr_eq(checked, &revision));
                context.blocked_revision = Some(revision.clone());
                context.verified_revision = Some(revision);
                let status = context.attention;
                let context_location = context.location.clone();
                owner.cache_attention(&context);
                drop(context);
                if changed {
                    self.publish(&session, status, context_location);
                }
                return DriveState::Idle;
            }
            if context
                .blocked_revision
                .as_ref()
                .is_some_and(|blocked| Arc::ptr_eq(blocked, &revision))
            {
                owner.cache_attention(&context);
                drop(context);
                return DriveState::Idle;
            }
            context.blocked_revision = None;
            if context.attention == MailAttentionInfo::RebindRequired {
                context.attention = MailAttentionInfo::Unavailable;
            }
            if coalesced {
                if !context.wake.take() {
                    owner.cache_attention(&context);
                    // Retain the bit until the monotonic coalescing interval expires.
                    return DriveState::Advanced;
                }
                break;
            }
            let has_wait = owner.wait.lock().is_ok_and(|wait| wait.is_some());
            if has_wait || !context.wake.pending() || context.wake.take() {
                break;
            }
            // Release for one coalescing delay, then recheck every fence.
            drop(context);
            tokio::time::sleep(std::time::Duration::from_millis(
                grapt_runtime::mailbox::presentation::MAIL_WAKE_INTERVAL_MS as u64,
            ))
            .await;
            context = owner.context.clone().lock_owned().await;
            coalesced = true;
        }
        let location = context.location.clone();
        if context.owns_runtime() && context.require_current().is_err() {
            // Rebinding invalidates the old receipt before the new binding opens.
            // Failure remains unavailable; no timed retry or takeover is invented.
            context.close_pending = true;
            context.attention = if matches!(context.current_binding(), Err(E::Capability)) {
                context.blocked_revision = Some(revision.clone());
                context.require_rebind();
                MailAttentionInfo::RebindRequired
            } else {
                MailAttentionInfo::Unavailable
            };
            let status = context.attention;
            context.verified_revision = Some(revision);
            owner.cache_attention(&context);
            drop(context);
            self.publish(&session, status, location.clone());
            return DriveState::Advanced;
        }
        let retry = owner
            .recovery
            .lock()
            .ok()
            .and_then(|mut recovery| recovery.begin(&session));
        owner.cache_attention(&context);
        let (mut context, advanced) = owner.converge(context, &revision).await;
        if let Some(retry) = retry {
            retry.finish(match &advanced {
                Some(Ok(observed)) if observed.live => "live",
                Some(Err(_)) => "failed",
                _ => "pending",
            });
        }
        let Some(advanced) = advanced else {
            owner.cache_attention(&context);
            return DriveState::Idle;
        };
        let observed = match advanced {
            Ok(observed) => observed,
            Err(error) => {
                if matches!(error, E::Capability) {
                    context.require_rebind();
                }
                if let Some(action) = context.recovery_action {
                    let write_pending = context.runtime.as_ref().is_some_and(|runtime| {
                        runtime.delivery_work_pending() || runtime.delivery_debt_pending()
                    });
                    if let Ok(mut recovery) = owner.recovery.lock() {
                        context.close_pending = recovery.escalate(action, write_pending)
                            == super::recovery::Action::Reconnect;
                        recovery.defer(&context.session);
                    } else {
                        context.close_pending = action == super::recovery::Action::Reconnect;
                        context.blocked_revision = Some(revision.clone());
                    }
                } else if context.slow_retry {
                    // Needs attention now, but may clear from the source side:
                    // close, then reobserve after one slow wait.
                    if let Ok(mut recovery) = owner.recovery.lock() {
                        context.stop_for_slow_retry(&mut recovery);
                    } else {
                        context.close_pending = true;
                        context.blocked_revision = Some(revision.clone());
                    }
                } else {
                    context.close_pending = true;
                    context.blocked_revision = Some(revision.clone());
                }
                context.attention = if context.blocked_revision.is_none() && context.slow_stopped {
                    MailAttentionInfo::Stopped
                } else if context.blocked_revision.is_none() {
                    MailAttentionInfo::Retrying
                } else {
                    context.blocked_attention()
                };
                let status = context.attention;
                context.verified_revision = Some(revision.clone());
                owner.cache_attention(&context);
                drop(context);
                self.publish(&session, status, location.clone());
                return DriveState::Advanced;
            }
        };
        let live = observed.live;
        let mut restored = observed.restored;
        if live {
            context.slow_stopped = false;
            if let Ok(mut recovery) = owner.recovery.lock() {
                restored |= recovery.recovering();
                recovery.restored();
            }
        }
        if live && restored {
            // Completed authenticated observation can resume a native input
            // held for connection recovery, including on the same subscription.
            if owner.connection_restored().is_err() {
                context.close_pending = true;
                context.blocked_revision = Some(revision.clone());
                return DriveState::Advanced;
            }
        }
        let location = context.location.clone();
        let status = context
            .attention_snapshot(live)
            .unwrap_or(MailAttentionInfo::Unavailable);
        let changed = context.attention != status || context.verified_revision.is_none();
        context.attention = status;
        context.verified_revision = Some(revision.clone());
        // Equal pending counts can describe entirely different rows. A source
        // signal, rather than a count transition, requests the admission scan.
        if live && std::mem::take(&mut context.wake_scan_requested) {
            self.changed.notify_one();
        }
        if changed
            && matches!(status, MailAttentionInfo::Ready { pending, .. } if pending.get() > 0)
        {
            context.wake.actionable_change();
            let _coalesced = context.wake.take();
        }
        let has_wait = match owner.wait.lock() {
            Ok(mut wait) => {
                if wait.is_none() {
                    *wait = context
                        .runtime
                        .as_mut()
                        .and_then(|runtime| runtime.next_change().ok())
                        .map(|future| MailWait::Waiting(Box::pin(future)));
                }
                wait.is_some()
            }
            Err(_) => false,
        };
        let delivery_work = context
            .runtime
            .as_ref()
            .is_some_and(HttpMailbox::delivery_work_pending);
        owner.cache_attention(&context);
        drop(context);
        if changed {
            self.publish(&session, status, location.clone());
            self.changed.notify_one();
        }
        if !live && status != MailAttentionInfo::Disabled {
            // A finite recovery slice yields to prompt work, then keeps its
            // existing progress. This is fair work, not a transport retry.
            tokio::task::yield_now().await;
            return DriveState::Advanced;
        }
        if !has_wait {
            return DriveState::Idle;
        }
        let ready = poll_fn(|cx| {
            let Ok(mut wait) = owner.wait.lock() else {
                return Poll::Ready(false);
            };
            match wait.as_mut() {
                Some(MailWait::Waiting(future)) => match future.as_mut().poll(cx) {
                    Poll::Ready(result) => {
                        *wait = Some(MailWait::Received(result));
                        Poll::Ready(true)
                    }
                    // Retain the owned advisory wait while one finite debt slice
                    // yields back to the same service driver, without a new task.
                    Poll::Pending if delivery_work => Poll::Ready(false),
                    Poll::Pending => Poll::Pending,
                },
                Some(MailWait::Received(_)) => Poll::Ready(true),
                None => Poll::Ready(false),
            }
        })
        .await;
        if !ready {
            return DriveState::Advanced;
        }
        let mut context = owner.context.lock().await;
        let result = owner
            .wait
            .lock()
            .ok()
            .and_then(|mut wait| match wait.take() {
                Some(MailWait::Received(result)) => Some(result),
                other => {
                    *wait = other;
                    None
                }
            });
        let Some(result) = result else {
            return DriveState::Advanced;
        };
        match result {
            Ok(signal) => {
                if let Some(runtime) = context.runtime.as_mut() {
                    if runtime.observe_change(signal) {
                        context.wake.actionable_change();
                        context.wake_before = None;
                        context.wake_scan_requested = true;
                        // An advisory asks for observation; it cannot
                        // reauthorize a cancelled canonical input.
                    }
                }
            }
            Err(failure) => {
                // A quiet advisory wait's deadline renews the one owned poll,
                // keeping the authenticated snapshot, unless the door no
                // longer answers for it.
                let retryable = if failure.cause
                    == grapt_mail_transport::operation::OperationCause::Connection(
                        grapt_mail_transport::operation::ConnectionFailure::Deadline,
                    ) {
                    match probe::after_deadline(&mut context, &owner.recovery).await {
                        probe::Liveness::Held => return DriveState::Advanced,
                        probe::Liveness::Lost { retryable } => retryable,
                    }
                } else {
                    let retryable = context
                        .runtime
                        .as_ref()
                        .is_some_and(|runtime| runtime.change_failure_is_retryable(&failure))
                        || super::recovery::advisory_action(&context, &failure).is_some();
                    let _refusal =
                        context.transport_refusal("observation", RemoteMailboxError::Port(failure));
                    retryable
                };
                context.close_pending = true;
                if retryable {
                    if let Ok(mut recovery) = owner.recovery.lock() {
                        recovery.defer(&context.session);
                    }
                } else {
                    context.blocked_revision = Some(revision.clone());
                }
                context.attention = if context.blocked_revision.is_none() {
                    MailAttentionInfo::Retrying
                } else {
                    context.blocked_attention()
                };
                let status = context.attention;
                context.verified_revision = Some(revision.clone());
                owner.cache_attention(&context);
                drop(context);
                self.publish(&session, status, location.clone());
                // Driver closes at its next safe boundary, then a transient
                // wait can restore a complete snapshot. No signal or count is
                // invented from a deadline; terminal takeover/auth stays parked.
            }
        }
        DriveState::Advanced
    }
    /// Close requests run BETWEEN selects. Prompt arrival/service stop cannot
    /// drop a by-value binding during close and lose its join result. Every
    /// port close has the accepted five-second bound; all owners run together.
    pub(in crate::dogfood) async fn close_pending(&self) {
        let Ok(owners) = self.snapshot() else {
            return;
        };
        let mut work: Vec<OwnedWork<'_>> = owners
            .into_iter()
            .filter_map(|(session, owner)| {
                let mut context = owner.context.clone().try_lock_owned().ok()?;
                if !context.close_pending {
                    return None;
                }
                Some(Box::pin(async move {
                    if context.close_pending {
                        let closing_revision = context
                            .verified_revision
                            .clone()
                            .or_else(|| owner.revision().ok());
                        let wait_closed = owner.cancel_wait();
                        let closed = context.close_transport().await;
                        context.close_failed |= wait_closed.is_err();
                        context.close_cleanup_failed |= wait_closed.is_err();
                        context.close_pending = false;
                        self.changed.notify_one();
                        // Losing the door cannot confirm its old close, but a
                        // transient loss permits fresh authenticated observation.
                        // Terminal refusal and failed local cleanup keep fencing.
                        if wait_closed.is_err()
                            || closed
                                .as_ref()
                                .err()
                                .is_some_and(|error| super::recovery::action(error).is_none())
                        {
                            if context.blocked_revision.is_none() {
                                context.blocked_revision = closing_revision;
                            }
                            context.attention = context.blocked_attention();
                            context.verified_revision = context.blocked_revision.clone();
                            let status = context.attention;
                            let location = context.location.clone();
                            drop(context);
                            self.publish(&session, status, location);
                        }
                    }
                }) as OwnedWork<'_>)
            })
            .collect();
        poll_fn(|cx| {
            let mut index = 0;
            while index < work.len() {
                if work[index].as_mut().poll(cx).is_ready() {
                    drop(work.swap_remove(index));
                } else {
                    index += 1;
                }
            }
            if work.is_empty() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
        if let Ok(mut owners) = self.owners.lock() {
            self.prune_retired(&mut owners);
        }
    }
    pub(in crate::dogfood) async fn close(&self) -> Result<(), String> {
        let owners = self.snapshot()?;
        let mut work: Vec<CloseWork> = owners
            .into_iter()
            .map(|(_, owner)| {
                Box::pin(async move {
                    {
                        let wait_closed = owner.cancel_wait();
                        let mut context = owner.context.lock().await;
                        context.attention = MailAttentionInfo::Unavailable;
                        context.verified_revision = None;
                        let closed = context.finish().await;
                        if context.close_cleanup_failed || wait_closed.is_err() {
                            Err("an earlier native inbox close was not confirmed".into())
                        } else {
                            closed
                        }
                    }
                }) as _
            })
            .collect();
        let mut failed = self.retirement_cleanup_failed.load(Ordering::SeqCst);
        poll_fn(|cx| {
            let mut index = 0;
            while index < work.len() {
                match work[index].as_mut().poll(cx) {
                    Poll::Ready(result) => {
                        failed |= result.is_err();
                        drop(work.swap_remove(index));
                    }
                    Poll::Pending => index += 1,
                }
            }
            if work.is_empty() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
        if failed {
            Err("native inbox shutdown could not be confirmed".into())
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
pub(in crate::dogfood::native_mail) mod tests;
