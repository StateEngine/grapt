//! Bounded volatile SSE fan-out for the Dogfood-0 event closure.

use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderValue, Request, Response, StatusCode};
use axum::response::IntoResponse;
use hyper::body::Frame;
use tokio::sync::mpsc;

use crate::{ApiRefusal, ApiRouterBuilder, RouteMethod, RoutePath, SealedPrincipal};

use super::capture::{CaptureLiveState, CaptureStreamTerminal};
use super::capture_wire::{capture_interested, frame_analysis};
use super::events::{DogfoodEvent, ServerConnectedEvent};
use super::routes::{invalid_request_response, unauthorized_response};
use super::{encode_page, query, U3aLimitValues, U3aLimits, U3A_PRODUCTION_LIMITS};
use grapt_types::CaptureInterest;

mod approvals;
pub use approvals::ApprovalWatch;
use approvals::{ApprovalDrop, Approver, SubscriberRegistry};
pub use grapt_stream::limits::{
    SseError, SseLimitValues, SseLimits, MAX_SSE_EVENT_BYTES, MAX_SSE_FRAME_BYTES,
    MAX_SSE_PENDING_EVENTS, MAX_SSE_SUBSCRIBERS,
};
use grapt_window_lease::{WindowClaim, WindowWatch};
pub const DOGFOOD_EVENT_PATH: &str = "/api/event";
use grapt_window_lease::DEFAULT_CLOSE_GRACE;
mod positions;
pub use grapt_stream::positions::MAX_POSITIONED_FRAME_BYTES;
mod provisional;
mod subscribe;
pub use grapt_stream::provisional::{
    ProvisionalBlock, ProvisionalChange, ProvisionalKind, ProvisionalText,
};
use grapt_stream::reconciles::ReconcileCounters;
pub use grapt_stream::reconciles::{ReconcileCounts, ReconcileReason};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishReceipt {
    pub delivered: usize,
    pub overflowed: usize,
    pub disconnected: usize,
}

/// The fan-out interest. Both are checked at offer time, so a frame never
/// pays a queue slot for a subscriber it does not address.
enum SubscriberInterest {
    V2(query::EventLocationQuery),
    Capture(CaptureInterest),
}

enum SubscriberSink {
    Http {
        sender: mpsc::Sender<Bytes>,
        overflowed: Arc<AtomicBool>,
    },
    Host(Arc<HostEventQueue>),
    Capture {
        sender: mpsc::Sender<Bytes>,
        overflowed: Arc<AtomicBool>,
        live: Arc<CaptureLiveState>,
    },
}

struct SubscriberSender {
    approver: Option<Approver>,
    sink: SubscriberSink,
    interest: SubscriberInterest,
    /// Opted into stream positions; see `sse/positions.rs`.
    positions: bool,
}

/// One bounded capture subscription: the frame queue plus the overflow
/// latch the stream body reads. The sender lives in the fan-out; the body
/// drains.
#[derive(Debug)]
pub struct CaptureSubscription {
    pub receiver: mpsc::Receiver<Bytes>,
    pub overflowed: Arc<AtomicBool>,
}

impl core::fmt::Debug for SubscriberSender {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("SubscriberSender")
            .field("closed", &self.sink.is_closed())
            .finish()
    }
}

impl SubscriberSink {
    fn is_invalidated(&self) -> bool {
        match self {
            Self::Http { overflowed, .. } | Self::Capture { overflowed, .. } => {
                overflowed.load(Ordering::Acquire)
            }
            Self::Host(_) => false,
        }
    }
    fn is_closed(&self) -> bool {
        match self {
            Self::Http { sender, .. } => sender.is_closed(),
            Self::Host(queue) => queue.is_closed(),
            Self::Capture { sender, .. } => sender.is_closed(),
        }
    }

    fn offer(&self, frame: Bytes) -> OfferOutcome {
        match self {
            Self::Http { sender, overflowed } => match sender.try_send(frame) {
                Ok(()) => OfferOutcome::Delivered,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    overflowed.store(true, Ordering::Release);
                    OfferOutcome::Overflowed
                }
                Err(mpsc::error::TrySendError::Closed(_)) => OfferOutcome::Disconnected,
            },
            Self::Host(queue) => queue.offer(frame),
            Self::Capture {
                sender,
                overflowed,
                live,
            } => {
                // Queue protection only: a coordinate the stream fence
                // already holds is presented truth, not a queued frame. The
                // authoritative exactly-once gate stays in the stream body.
                if let Some((coordinate, _)) = frame_analysis(&frame) {
                    if live.holds(&coordinate.aggregate, coordinate.seq) {
                        return OfferOutcome::Delivered;
                    }
                }
                match sender.try_send(frame) {
                    Ok(()) => OfferOutcome::Delivered,
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        overflowed.store(true, Ordering::Release);
                        OfferOutcome::Overflowed
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => OfferOutcome::Disconnected,
                }
            }
        }
    }

    fn invalidate(&self) {
        match self {
            Self::Http { overflowed, .. } => overflowed.store(true, Ordering::Release),
            Self::Host(queue) => queue.invalidate(),
            Self::Capture {
                overflowed, live, ..
            } => {
                overflowed.store(true, Ordering::Release);
                live.arm_terminal(CaptureStreamTerminal::Invalidated);
            }
        }
    }

    fn reconcile(&self) {
        match self {
            Self::Host(queue) => queue.reconcile(),
            // The queued frames stay valid: the body drains them, and the
            // armed terminal names the close.
            Self::Capture { live, .. } => live.arm_terminal(CaptureStreamTerminal::Reconciled),
            Self::Http { .. } => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OfferOutcome {
    Delivered,
    Overflowed,
    Disconnected,
}

#[derive(Debug)]
struct HostEventQueue {
    maximum: usize,
    state: Mutex<HostEventQueueState>,
    changed: Condvar,
}

#[derive(Debug)]
struct HostEventQueueState {
    frames: VecDeque<Bytes>,
    overflowed: bool,
    closed: bool,
}

impl HostEventQueue {
    fn new(maximum: usize, connected: Bytes) -> Self {
        let mut frames = VecDeque::with_capacity(maximum);
        frames.push_back(connected);
        Self {
            maximum,
            state: Mutex::new(HostEventQueueState {
                frames,
                overflowed: false,
                closed: false,
            }),
            changed: Condvar::new(),
        }
    }

    fn offer(&self, frame: Bytes) -> OfferOutcome {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return OfferOutcome::Disconnected,
        };
        if state.closed {
            return OfferOutcome::Disconnected;
        }
        if state.frames.len() >= self.maximum {
            state.overflowed = true;
            state.closed = true;
            self.changed.notify_all();
            return OfferOutcome::Overflowed;
        }
        state.frames.push_back(frame);
        self.changed.notify_one();
        OfferOutcome::Delivered
    }

    /// Best-effort queueing for hydration frames: a full or closed queue
    /// loses the frame without ending the stream.
    fn offer_soft(&self, frame: Bytes) -> OfferOutcome {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return OfferOutcome::Disconnected,
        };
        if state.closed {
            return OfferOutcome::Disconnected;
        }
        if state.frames.len() >= self.maximum {
            return OfferOutcome::Overflowed;
        }
        state.frames.push_back(frame);
        self.changed.notify_one();
        OfferOutcome::Delivered
    }

    fn is_closed(&self) -> bool {
        self.state.lock().map_or(true, |state| state.closed)
    }

    fn invalidate(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.overflowed = true;
            state.closed = true;
            self.changed.notify_all();
        }
    }

    fn reconcile(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
            self.changed.notify_all();
        }
    }

    fn next(&self) -> Result<Option<Bytes>, SseError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| SseError::RegistryUnavailable)?;
        loop {
            if state.overflowed {
                return Err(SseError::SubscriberOverflow);
            }
            if let Some(frame) = state.frames.pop_front() {
                return Ok(Some(frame));
            }
            if state.closed {
                return Ok(None);
            }
            state = self
                .changed
                .wait(state)
                .map_err(|_| SseError::RegistryUnavailable)?;
        }
    }
}

/// One bounded blocking subscription for a supervised host adapter.
///
/// It shares the exact fan-out and overflow policy used by HTTP SSE. Closing a
/// generation wakes a pending `next` without a timer or an ambient polling loop.
#[derive(Debug)]
pub struct DogfoodEventSubscription {
    queue: Arc<HostEventQueue>,
    hydration_frames: usize,
}

impl DogfoodEventSubscription {
    /// How many frames after `server.connected` restate current state; every
    /// later frame is live.
    pub fn hydration_frames(&self) -> usize {
        self.hydration_frames
    }

    pub fn next_frame(&self) -> Result<Option<Bytes>, SseError> {
        self.queue.next()
    }

    pub fn cancel(&self) {
        self.queue.reconcile();
    }
}

impl Drop for DogfoodEventSubscription {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Why the hub ended advisory streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEndCause {
    /// A committed reconciliation boundary: queued frames drain, then EOF.
    Reconcile,
    /// A projection or publication failure: streams close at once.
    Invalidate,
    /// A subscriber's bounded queue was full, so it was dropped.
    Overflow,
}

impl StreamEndCause {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reconcile => "reconcile",
            Self::Invalidate => "invalidate",
            Self::Overflow => "overflow",
        }
    }
}

/// One time the hub ended subscribers, for diagnostics only.
#[derive(Debug, Clone, Copy)]
pub struct StreamEnd<'a> {
    pub cause: StreamEndCause,
    /// How many subscribers were ended.
    pub subscribers: usize,
    /// Why, as the caller stated it; for an overflow, the event being sent.
    pub reason: &'a str,
    /// Fixed global-recovery category; an individual overflow has none.
    pub reconcile_reason: Option<ReconcileReason>,
}

/// Told about every time the hub ends subscribers. Runs after the
/// subscriber lock is released, and must not block for long.
pub type StreamEndObserver = Box<dyn Fn(&StreamEnd<'_>) + Send + Sync>;

/// Supplies the last-known session-status snapshot a new subscriber receives
/// right after its connection frame. Runs while the subscribe path holds the
/// subscriber lock, so it must return a small bounded vec and never block.
pub type StatusHydrator = Box<dyn Fn() -> Vec<DogfoodEvent> + Send + Sync>;

/// Process-local fan-out. It keeps a bounded replay ring for resumes and no
/// durable history.
pub struct DogfoodEventHub {
    limits: SseLimits,
    connected: Bytes,
    subscribers: Arc<Mutex<SubscriberRegistry>>,
    /// Live blocks a snapshot may show. Its own lock, taken after the
    /// subscriber lock by a publish and alone by a reader, so no writer that
    /// holds the subscriber lock across a commit can make a snapshot wait.
    provisional: Mutex<grapt_stream::provisional::ProvisionalRegistry>,
    /// Which Sessions still have a watching window, and the signal a deadline
    /// owner waits on. A window that vanishes cannot announce itself, so its
    /// subscription ending is the only truthful signal.
    windows: Arc<WindowWatch>,
    ends: OnceLock<StreamEndObserver>,
    status_hydrator: OnceLock<StatusHydrator>,
    reconciles: ReconcileCounters,
    replay_counts: Arc<grapt_stream::replay::ReplayCounters>,
    approval_changes: Arc<tokio::sync::watch::Sender<()>>,
}

impl core::fmt::Debug for DogfoodEventHub {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("DogfoodEventHub")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl DogfoodEventHub {
    pub fn try_new(limits: SseLimits, connected: ServerConnectedEvent) -> Result<Self, SseError> {
        Self::with_close_grace(limits, connected, DEFAULT_CLOSE_GRACE)
    }

    /// The same hub with a deployment's own silent-loss grace.
    ///
    /// The grace is a property of the service, not of a window, so it is set
    /// once here; the deadline owner reads it back through [`WindowWatch`].
    pub fn with_close_grace(
        limits: SseLimits,
        connected: ServerConnectedEvent,
        grace: std::time::Duration,
    ) -> Result<Self, SseError> {
        let connected = encode_sse(&DogfoodEvent::ServerConnected(connected), limits)?;
        let registry = SubscriberRegistry::new(limits.subscribers());
        Ok(Self {
            limits,
            connected,
            replay_counts: registry.replay.counters(),
            subscribers: Arc::new(Mutex::new(registry)),
            provisional: Mutex::new(grapt_stream::provisional::ProvisionalRegistry::default()),
            windows: Arc::new(WindowWatch::with_grace(grace)),
            ends: OnceLock::new(),
            status_hydrator: OnceLock::new(),
            reconciles: ReconcileCounters::default(),
            approval_changes: Arc::new(tokio::sync::watch::channel(()).0),
        })
    }

    /// Install the one observer told why subscribers are ended. Returns false
    /// if one is already installed.
    pub fn observe_stream_ends(&self, observer: StreamEndObserver) -> bool {
        self.ends.set(observer).is_ok()
    }

    /// Install the one last-known session-status snapshot supplier. Returns
    /// false if one is already installed.
    pub fn install_status_hydrator(&self, hydrator: StatusHydrator) -> bool {
        self.status_hydrator.set(hydrator).is_ok()
    }

    /// The window registry every Session-named subscription attaches to, and
    /// the signal a deadline owner waits on for the next expiry.
    pub fn windows(&self) -> &Arc<WindowWatch> {
        &self.windows
    }

    /// Queue the snapshot frames for one new subscriber BEFORE it is
    /// registered, so a live edge published after the snapshot always lands
    /// after it. A frame that will not encode or does not fit ends the
    /// snapshot, and the error says which; the caller decides whether a
    /// partial snapshot may still stream.
    fn queue_hydration(
        &self,
        interest: &query::EventLocationQuery,
        offer: &mut impl FnMut(Bytes) -> bool,
    ) -> Result<(), SseError> {
        let Some(hydrator) = self.status_hydrator.get() else {
            return Ok(());
        };
        for event in hydrator() {
            let location = event.attention_location();
            if location.is_some_and(|location| !v2_interested(interest, Some(location))) {
                continue;
            }
            let frame = encode_sse(&event, self.limits).map_err(|_| SseError::Encode)?;
            if !offer(frame) {
                return Err(SseError::SubscriberOverflow);
            }
        }
        Ok(())
    }

    /// Every global close, including immediate invalidation, requires durable recovery.
    /// Subscriber-local overflow is not a global reconcile.
    pub fn reconcile_counts(&self) -> ReconcileCounts {
        self.reconciles.snapshot()
    }

    fn report(
        &self,
        cause: StreamEndCause,
        subscribers: usize,
        reason: &str,
        reconcile_reason: Option<ReconcileReason>,
    ) {
        if subscribers == 0 && reconcile_reason.is_none() {
            return;
        }
        if let Some(observer) = self.ends.get() {
            observer(&StreamEnd {
                cause,
                subscribers,
                reason,
                reconcile_reason,
            });
        }
    }

    pub fn publish(&self, event: &DogfoodEvent) -> Result<PublishReceipt, SseError> {
        self.publish_scoped(event, None, || true, || ((), None), |()| {})
    }

    /// Recheck an advisory projection under the subscriber lock, in the same
    /// lock order as hydration. A false condition offers no frame.
    pub fn publish_if(
        &self,
        event: &DogfoodEvent,
        current: impl FnOnce() -> bool,
    ) -> Result<PublishReceipt, SseError> {
        self.publish_scoped(event, None, current, || ((), None), |()| {})
    }

    /// Include the location envelope used by retained clients and route the
    /// advisory frame to subscribers interested in that location.
    pub fn publish_at(
        &self,
        event: &DogfoodEvent,
        location: &grapt_types::LocationRef,
    ) -> Result<PublishReceipt, SseError> {
        self.publish_scoped(event, Some(location), || true, || ((), None), |()| {})
    }

    /// Encode once, then offer under the subscriber lock. `stage` runs after
    /// that lock is taken and before any offer; what it holds (the live-block
    /// registry's guard) stays held through the offers and `offered`, and is
    /// released with the subscriber lock, before any callback below. It also
    /// names a delta's offset for the positioned frame.
    fn publish_scoped<G>(
        &self,
        event: &DogfoodEvent,
        location: Option<&grapt_types::LocationRef>,
        current: impl FnOnce() -> bool,
        stage: impl FnOnce() -> (G, Option<u64>),
        offered: impl FnOnce(&mut G),
    ) -> Result<PublishReceipt, SseError> {
        // Native attention always uses its Session owner's location. Generic
        // callers cannot make it global or substitute another location.
        let location = event.attention_location().or(location);
        let frame = match location {
            Some(location) => encode_sse(
                &ScopedEvent {
                    event,
                    location: ScopedLocation {
                        directory: location.directory(),
                        workspace_id: location.workspace().map(|id| id.as_str()),
                    },
                },
                self.limits,
            )?,
            None => encode_sse(event, self.limits)?,
        };
        let mut subscribers = self
            .subscribers
            .lock()
            .map_err(|_| SseError::RegistryUnavailable)?;
        let mut receipt = PublishReceipt {
            delivered: 0,
            overflowed: 0,
            disconnected: 0,
        };
        if !current() {
            return Ok(receipt);
        }
        let (mut staged, from) = stage();
        // Every offered frame is positioned once, at one position shared by
        // every positions subscriber, and kept for resumes even when none is
        // attached: a disconnected client's gap has positions only this way.
        let limits = self.limits;
        let positioned =
            positions::frame(&mut subscribers.positions, event, &frame, from, limits).ok();
        match &positioned {
            Some(positioned) => {
                let seq = subscribers.positions.seq();
                subscribers.replay.push(grapt_stream::replay::ReplayEntry {
                    seq,
                    location: location.cloned(),
                    frame: positioned.clone(),
                });
            }
            // Positions subscribers are ended below; no resume may cross it.
            None => subscribe::replay_barrier(&mut subscribers),
        }
        subscribers.retain(|subscriber| {
            if subscriber.sink.is_closed() {
                receipt.disconnected += 1;
                return false;
            }
            if !interested(&subscriber.interest, location) {
                return true;
            }
            let offered = match (subscriber.positions, &positioned) {
                (false, _) => frame.clone(),
                (true, Some(positioned)) => positioned.clone(),
                // Unrepresentable in positioned form: end it visibly.
                (true, None) => {
                    subscriber.sink.invalidate();
                    receipt.overflowed += 1;
                    return false;
                }
            };
            match subscriber.sink.offer(offered) {
                OfferOutcome::Delivered => {
                    receipt.delivered += 1;
                    true
                }
                OfferOutcome::Overflowed => {
                    receipt.overflowed += 1;
                    false
                }
                OfferOutcome::Disconnected => {
                    receipt.disconnected += 1;
                    false
                }
            }
        });
        offered(&mut staged);
        subscribers.refresh();
        drop(staged);
        drop(subscribers);
        self.approvals_changed();
        self.report(
            StreamEndCause::Overflow,
            receipt.overflowed,
            event.kind(),
            None,
        );
        Ok(receipt)
    }

    /// A projection failure cannot undo its committed fact. Close the advisory
    /// streams so clients reconnect and hydrate durable truth instead of keeping
    /// a silently incomplete live view.
    pub fn invalidate(&self) {
        self.invalidate_because("unstated");
    }

    /// [`Self::invalidate`], telling the stream-end observer why.
    pub fn invalidate_because(&self, reason: &str) {
        self.reconciles.record(ReconcileReason::ProjectionFailure);
        let mut subscribers = match self.subscribers.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        };
        for subscriber in subscribers.iter() {
            subscriber.sink.invalidate();
        }
        subscribe::replay_barrier(&mut subscribers);
        let ended = subscribers.len();
        subscribers.clear();
        subscribers.refresh();
        drop(subscribers);
        self.approvals_changed();
        self.report(
            StreamEndCause::Invalidate,
            ended,
            reason,
            Some(ReconcileReason::ProjectionFailure),
        );
    }

    /// [`Self::invalidate_because`] for a failure about one Session. Frames
    /// reach subscribers by location interest, not by the Session a window is
    /// bound to, so a window bound to another Session may already show this
    /// one's state (its status, for one). Every subscriber is ended, so none
    /// keeps a stale view without a gap signal.
    pub fn invalidate_session_because(&self, session: &grapt_types::SessionId, reason: &str) {
        self.invalidate_because(&format!("session {session}: {reason}"));
    }

    /// End an advisory stream at a committed reconciliation boundary. Unlike
    /// a failed projection, its queued frames remain valid: dropping senders
    /// drains those bounded frames before EOF prompts durable hydration.
    pub fn reconcile(&self) {
        self.reconcile_because(ReconcileReason::UnpublishedAdvisoryNotice, "unstated");
    }

    /// [`Self::reconcile`], telling the stream-end observer why.
    pub fn reconcile_because(&self, category: ReconcileReason, reason: &str) {
        self.reconciles.record(category);
        self.finish_streams(Some(category), reason);
    }

    /// Drain valid queued notices before EOF during an owned service stop.
    pub fn shutdown(&self) {
        self.finish_streams(None, "service-shutdown");
    }

    fn finish_streams(&self, category: Option<ReconcileReason>, reason: &str) {
        let mut subscribers = match self.subscribers.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        };
        // A positions subscriber is told to re-read before its stream ends.
        let resync = if category.is_some() && subscribers.iter().any(|held| held.positions) {
            let limits = self.limits;
            positions::resync(&mut subscribers.positions, limits).ok()
        } else {
            None
        };
        for subscriber in subscribers.iter() {
            if let (true, Some(resync)) = (subscriber.positions, &resync) {
                let _offered = subscriber.sink.offer(resync.clone());
            }
            subscriber.sink.reconcile();
        }
        subscribe::replay_barrier(&mut subscribers);
        let ended = subscribers.len();
        subscribers.clear();
        subscribers.refresh();
        drop(subscribers);
        self.approvals_changed();
        self.report(StreamEndCause::Reconcile, ended, reason, category);
    }

    #[cfg(test)]
    fn subscribe(&self) -> Result<SseBody, SseError> {
        self.subscribe_window_at(query::EventLocationQuery::Absent, None, None)
    }

    #[cfg(test)]
    fn subscribe_at(&self, interest: query::EventLocationQuery) -> Result<SseBody, SseError> {
        self.subscribe_window_at(interest, None, None)
    }

    #[cfg(test)]
    fn subscribe_approver_at(
        &self,
        interest: query::EventLocationQuery,
        approver: Option<Approver>,
    ) -> Result<SseBody, SseError> {
        self.subscribe_window_at(interest, approver, None)
    }

    /// Subscribe one HTTP window. A named Session registers the window before
    /// any frame is queued, so a stop decision can never precede a window that
    /// is already watching.
    #[cfg(test)]
    fn subscribe_window_at(
        &self,
        interest: query::EventLocationQuery,
        approver: Option<Approver>,
        session: Option<grapt_types::SessionId>,
    ) -> Result<SseBody, SseError> {
        self.subscribe_window(interest, approver, session, Vec::new(), false, None)
    }

    /// Subscribe a supervised host adapter to the same bounded event owner as
    /// HTTP SSE. The returned handle owns cancellation and exposes no sender.
    pub fn subscribe_host(&self) -> Result<DogfoodEventSubscription, SseError> {
        let mut subscribers = self
            .subscribers
            .lock()
            .map_err(|_| SseError::RegistryUnavailable)?;
        subscribers.retain(|subscriber| !subscriber.sink.is_closed());
        if subscribers.len() >= self.limits.subscribers() {
            return Err(SseError::SubscriberBound);
        }
        let queue = Arc::new(HostEventQueue::new(
            self.limits.pending_events(),
            self.connected.clone(),
        ));
        // A host adapter counts on the whole snapshot, so a partial one is
        // refused rather than streamed; the queue is never registered.
        let mut hydration_frames = 0;
        self.queue_hydration(&query::EventLocationQuery::Absent, &mut |frame| {
            let delivered = queue.offer_soft(frame) == OfferOutcome::Delivered;
            hydration_frames += usize::from(delivered);
            delivered
        })?;
        subscribers.push(SubscriberSender {
            approver: None,
            sink: SubscriberSink::Host(Arc::clone(&queue)),
            interest: SubscriberInterest::V2(query::EventLocationQuery::Absent),
            positions: false,
        });
        Ok(DogfoodEventSubscription {
            queue,
            hydration_frames,
        })
    }

    /// Register the capture sink for one stream. This must run BEFORE the
    /// stream first durable read: a frame committed after this point is
    /// either in this queue or in the durable truth the replay serves,
    /// never in the gap between the two.
    pub(super) fn subscribe_capture(
        &self,
        interest: CaptureInterest,
        live: Arc<CaptureLiveState>,
    ) -> Result<CaptureSubscription, SseError> {
        let mut subscribers = self
            .subscribers
            .lock()
            .map_err(|_| SseError::RegistryUnavailable)?;
        subscribers.retain(|subscriber| !subscriber.sink.is_closed());
        if subscribers.len() >= self.limits.subscribers() {
            return Err(SseError::SubscriberBound);
        }
        let (sender, receiver) = mpsc::channel(self.limits.pending_events());
        let overflowed = Arc::new(AtomicBool::new(false));
        subscribers.push(SubscriberSender {
            approver: None,
            sink: SubscriberSink::Capture {
                sender,
                overflowed: Arc::clone(&overflowed),
                live,
            },
            interest: SubscriberInterest::Capture(interest),
            positions: false,
        });
        Ok(CaptureSubscription {
            receiver,
            overflowed,
        })
    }

    /// The hub frame bounds. The capture stream encodes its control frames
    /// under the exact limits its live frames were already checked against.
    pub fn limits(&self) -> SseLimits {
        self.limits
    }
}

fn interested(interest: &SubscriberInterest, location: Option<&grapt_types::LocationRef>) -> bool {
    match interest {
        SubscriberInterest::V2(v2) => v2_interested(v2, location),
        SubscriberInterest::Capture(capture) => capture_interested(
            capture,
            location.map(|location| {
                (
                    location.directory(),
                    location.workspace().map(|workspace| workspace.as_str()),
                )
            }),
        ),
    }
}

fn v2_interested(
    interest: &query::EventLocationQuery,
    location: Option<&grapt_types::LocationRef>,
) -> bool {
    let (
        query::EventLocationQuery::Value {
            directory,
            workspace,
        },
        Some(location),
    ) = (interest, location)
    else {
        return true;
    };
    if workspace.as_ref().map(|id| id.as_inner()) != location.workspace() {
        return false;
    }
    directory.as_ref().is_none_or(|directory| {
        grapt_types::LocationRef::new(directory.as_str(), location.workspace().cloned())
            .is_ok_and(|interest| interest == *location)
    })
}

struct SseBody {
    approval_changes: Option<ApprovalDrop>,
    receiver: mpsc::Receiver<Bytes>,
    overflowed: Arc<AtomicBool>,
    terminated: bool,
    /// The window this stream is, when it named a Session. Dropping the body is
    /// what tells the service the window is gone, so the value is held for its
    /// `Drop` and never read.
    _windows: Vec<WindowClaim>,
}

impl Drop for SseBody {
    fn drop(&mut self) {
        if let Some(approval) = &self.approval_changes {
            let mut registry = match approval.subscribers.lock() {
                Ok(held) => held,
                Err(poisoned) => poisoned.into_inner(),
            };
            self.receiver.close();
            registry.refresh();
            approval.changes.send_replace(());
        } else {
            self.receiver.close();
        }
    }
}

impl core::fmt::Debug for SseBody {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("SseBody")
            .field("overflowed", &self.overflowed.load(Ordering::Acquire))
            .field("terminated", &self.terminated)
            .finish()
    }
}

impl hyper::body::Body for SseBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if this.terminated {
            return Poll::Ready(None);
        }
        if this.overflowed.load(Ordering::Acquire) {
            this.terminated = true;
            this.receiver.close();
            return Poll::Ready(Some(Err(io::Error::other(
                "bounded SSE subscriber overflowed",
            ))));
        }
        match this.receiver.poll_recv(context) {
            Poll::Ready(Some(bytes)) => Poll::Ready(Some(Ok(Frame::data(bytes)))),
            Poll::Ready(None) => {
                this.terminated = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[derive(serde::Serialize)]
struct ScopedEvent<'a> {
    #[serde(flatten)]
    event: &'a DogfoodEvent,
    location: ScopedLocation<'a>,
}

#[derive(serde::Serialize)]
struct ScopedLocation<'a> {
    directory: &'a str,
    #[serde(rename = "workspaceID", skip_serializing_if = "Option::is_none")]
    workspace_id: Option<&'a str>,
}

pub(super) fn encode_sse<T: serde::Serialize>(
    event: &T,
    sse_limits: SseLimits,
) -> Result<Bytes, SseError> {
    let mut values: U3aLimitValues = U3A_PRODUCTION_LIMITS;
    values.page_bytes = sse_limits.event_bytes();
    let limits = U3aLimits::try_new(values).map_err(|_| SseError::EventBytes)?;
    let json = encode_page(event, limits).map_err(|_| SseError::EventBytes)?;
    let capacity = json.len().checked_add(23).ok_or(SseError::FrameBytes)?;
    if capacity > sse_limits.frame_bytes() {
        return Err(SseError::FrameBytes);
    }
    let mut frame = Vec::with_capacity(capacity);
    frame.extend_from_slice(b"event: message\n");
    frame.extend_from_slice(b"data: ");
    frame.extend_from_slice(&json);
    frame.extend_from_slice(b"\n\n");
    Ok(Bytes::from(frame))
}

mod route;
pub use route::register_dogfood_event_route;

pub(super) fn sse_response<T>(body: T) -> axum::response::Response
where
    T: hyper::body::Body<Data = Bytes, Error = io::Error> + Send + 'static,
{
    let mut response = Response::new(Body::new(body));
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.into_response()
}
#[cfg(test)]
#[path = "sse_tests.rs"]
mod tests;
