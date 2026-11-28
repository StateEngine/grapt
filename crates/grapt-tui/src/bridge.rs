//! Event delivery blocks on the socket; RPC work never owns terminal input.
use crate::catalog_gate::{CatalogGate, Gate};
use crate::config::Config;
use grapt_client::supervise::{self, ConnectionView, Event};
use grapt_client::{
    Bounds, Connection, FramePoll, HttpConnection, StreamCancellation, Transport, TransportError,
    WatchedSessions, WireRequest, WireResponse,
};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::{self, Receiver, SyncSender},
    Arc,
};
use tokio::time::Instant;

mod reader;
mod resume;
use reader::{read_events, Reader};
#[cfg(test)]
use resume::{authority_changed, catalog_changed};
use resume::{SharedCustody, Suppress};

/// Live frames the reader may queue while the UI draws. A full handoff makes
/// the reader wait, which stops it reading the socket: the stream is held
/// back, never dropped. At the 256 KiB frame bound this is at most 128 MiB.
const HANDOFF_FRAMES: usize = 512;

#[derive(Debug)]
struct Attachment {
    rpc: HttpConnection,
    cancel: Option<StreamCancellation>,
    valid: Arc<AtomicBool>,
}

#[derive(Debug)]
enum Delivery {
    Frame(FramePoll),
    // An ordered barrier in the event handoff, carrying the verified RPC peer
    // and the selection generation it was built for.
    Replacement(Result<Box<Attachment>, TransportError>, u64),
    /// A replacement abandoned because the selection moved during its
    /// handshake. It carries no attachment, so the backend only re-arms.
    Stale,
}

#[derive(Debug)]
pub struct Bridge {
    rpc: HttpConnection,
    frames: Receiver<Delivery>,
    gaps: Receiver<()>,
    reconnect: SyncSender<()>,
    /// A replacement was requested and its barrier is still ahead in the
    /// handoff. Queue bookkeeping only; the connection's state is `state`.
    replacement_requested: bool,
    /// The current stream broke its contract and was retired. Until a verified
    /// replacement of the current generation installs, `poll_frame` delivers
    /// nothing and `poll_reconnect` alone drains the retired stream's frames.
    retired: bool,
    /// The service is too old for this client. Terminal: no replacement is
    /// ever requested again, and every reconnect returns this refusal.
    too_old: Option<&'static str>,
    /// The Session this window watches, shared with the reader thread so every
    /// (re)subscription it opens names the same Session. `None` until a Session
    /// is selected: an unnamed subscription is not a window on anything.
    session: Arc<std::sync::Mutex<WatchedSessions>>,
    /// Counts selection changes, shared with the reader thread.
    ///
    /// A replacement takes time to build: connect, name a Session, read the
    /// subscription head. If the window selects another Session meanwhile, that
    /// replacement names the OLD one. Installing it would hand the service a
    /// lease for a Session this window has left and none for the one it now
    /// shows, so a reconnect would be counted as a window that is gone. The
    /// reader records the generation it started building under and the backend
    /// rejects a barrier from any other generation.
    generation: Arc<AtomicU64>,
    stream_cancel: Option<StreamCancellation>,
    /// The attachment's one state, written only here on the backend thread,
    /// in the order its frames, gaps and replacements are drained.
    state: supervise::Connection,
    /// Revocation fence for the current attachment, shared with its reader
    /// and any detached stop. It only ever goes from true to false: the
    /// reader revokes it the moment its stream closes, ahead of queued frames.
    fence: Arc<AtomicBool>,
    last_event: Option<Instant>,
    /// Frames waiting behind a session-catalog refresh.
    gate: CatalogGate,
    pub authority_dirty: Arc<AtomicBool>,
    pub sessions_dirty: Arc<AtomicBool>,
    pub(super) read_health: crate::read_health::Health,
    /// Resume custody shared with the reader, and the installed stream's
    /// binding, adopted only from the build of the attachment installed.
    custody: SharedCustody,
    binding: Option<grapt_client::client::StreamBinding>,
    /// An install built with the offer holds its refreshes until the client
    /// answers the first frame, and that one frame passes the catalog hold.
    answer: resume::Answer,
}

/// Independent command ownership, fenced to the original attachment.
#[derive(Debug)]
pub(super) struct StopPeer {
    rpc: HttpConnection,
    valid: Arc<AtomicBool>,
}

impl StopPeer {
    /// Blocking preflight: invoke only inside the stop worker, before sending.
    pub(super) fn verify(self) -> Result<HttpConnection, TransportError> {
        self.require_live()?;
        let identity = self.rpc.verify_pinned_product();
        self.require_live()?;
        identity.map_err(|error| TransportError::Unavailable {
            reason: format!("stop product verification failed: {error}; reconnect and retry stop"),
        })?;
        Ok(self.rpc)
    }

    fn require_live(&self) -> Result<(), TransportError> {
        // The attachment's own validity fences stop: a closed or replaced
        // attachment refuses, a live view that is merely resyncing does not.
        if self.valid.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(TransportError::Unavailable {
                reason: "stop connection changed; reconnect and retry stop".into(),
            })
        }
    }
}

/// A connection for one periodic read, fenced to the attachment it came
/// from: an answer that crosses a reconnect is refused, never applied.
#[derive(Debug)]
pub(crate) struct ReadPeer {
    rpc: HttpConnection,
    valid: Arc<AtomicBool>,
}

impl ReadPeer {
    /// The attachment this read is sent on, kept with its ticket.
    pub(crate) fn attachment(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.valid)
    }

    /// Blocking: invoke only on the read worker. An attachment revoked
    /// before the send, by a reconnect or a refusal, is sent nothing; one
    /// revoked during it has its answer refused.
    pub(crate) fn call(mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        let revoked = || TransportError::Unavailable {
            reason: "service reconnecting; read retried".into(),
        };
        if !self.valid.load(Ordering::Acquire) {
            return Err(revoked());
        }
        let response = self.rpc.call(request);
        if self.valid.load(Ordering::Acquire) {
            response
        } else {
            Err(revoked())
        }
    }
}

/// The Sessions a window watches, shared with every subscription it opens,
/// starting from the launch Session when one was named.
fn watched_session(config: &Config) -> Arc<std::sync::Mutex<WatchedSessions>> {
    Arc::new(std::sync::Mutex::new(WatchedSessions::showing(
        config
            .session
            .as_ref()
            .map(|session| session.as_inner().clone()),
    )))
}

/// The Sessions a window watches now. A poisoned lock still holds the last
/// state this process wrote, so it is read rather than lost.
fn current_watched(shared: &Arc<std::sync::Mutex<WatchedSessions>>) -> WatchedSessions {
    match shared.lock() {
        Ok(held) => held.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// The first subscription, naming the watched Session; a refusal is returned
/// so the bridge reports its cause rather than a quiet reconnect.
fn first_subscribe(
    stream: &mut HttpConnection,
    session: &Arc<std::sync::Mutex<WatchedSessions>>,
) -> Option<TransportError> {
    stream.adopt_watched(current_watched(session));
    stream.resubscribe();
    (!stream.subscribed())
        .then(|| stream.poll_frame().err())
        .flatten()
}

impl Bridge {
    pub fn open(mut config: Config, bounds: Bounds, wake: SyncSender<()>) -> Result<Self, String> {
        let (rpc, mut stream) = config.connect(bounds)?;
        // The launcher pins the initial attachment. Later recovery reads the
        // current original profile and verifies its newly published Product.
        config.expected = None;
        // A launch Session names the window from its very first subscription,
        // and the reader re-names it on every replacement it opens, so a
        // reconnect inside the grace keeps its attachment.
        let session = watched_session(&config);
        let refusal = first_subscribe(&mut stream, &session);
        let reader_session = Arc::clone(&session);
        let selection = Arc::new(AtomicU64::new(0));
        let reader_generation = Arc::clone(&selection);
        let stream_cancel = stream.stream_cancellation().map_err(|e| e.to_string())?;
        let (tx, frames) = mpsc::sync_channel(HANDOFF_FRAMES);
        let (gap, gaps) = mpsc::sync_channel(1);
        let (reconnect, retry) = mpsc::sync_channel(1);
        let attached = stream.subscribed();
        let fence = Arc::new(AtomicBool::new(attached));
        let reader_valid = fence.clone();
        let authority_dirty = Arc::new(AtomicBool::new(true));
        let sessions_dirty = Arc::new(AtomicBool::new(false));
        let (authority, sessions) = (authority_dirty.clone(), sessions_dirty.clone());
        let binding = stream.stream_binding().filter(|_| attached);
        let custody = SharedCustody::default();
        let reader = Reader {
            config,
            bounds,
            session: reader_session,
            generation: reader_generation,
            stream,
            valid: reader_valid,
            handoff: tx,
            wake,
            custody: Arc::clone(&custody),
            suppress: Suppress::default(),
            retry,
            gap,
            authority,
            sessions,
        };
        let _reader = std::thread::Builder::new()
            .name("tui-r-events".into())
            .spawn(move || read_events(reader))
            .map_err(|e| e.to_string())?;
        let mut bridge = Self::assembled(rpc, frames, reconnect);
        bridge.gaps = gaps;
        bridge.stream_cancel = stream_cancel;
        bridge.session = session;
        bridge.generation = selection;
        if !attached {
            bridge.state = supervise::Connection::Replacing { attempt: 1 };
        }
        bridge.report(refusal);
        bridge.fence = fence;
        bridge.authority_dirty = authority_dirty;
        bridge.sessions_dirty = sessions_dirty;
        bridge.custody = custody;
        bridge.binding = binding;
        Ok(bridge)
    }

    /// A live, attached bridge over `rpc`, fed by `frames`. Opening sets the
    /// reader's shared flags; tests adjust the fields they exercise.
    fn assembled(
        rpc: HttpConnection,
        frames: Receiver<Delivery>,
        reconnect: SyncSender<()>,
    ) -> Self {
        Self {
            rpc,
            frames,
            gaps: mpsc::sync_channel(1).1,
            reconnect,
            replacement_requested: false,
            retired: false,
            too_old: None,
            session: Arc::new(std::sync::Mutex::new(WatchedSessions::default())),
            generation: Arc::new(AtomicU64::new(0)),
            stream_cancel: None,
            state: supervise::Connection::Live,
            fence: Arc::new(AtomicBool::new(true)),
            last_event: None,
            gate: CatalogGate::default(),
            authority_dirty: Arc::new(AtomicBool::new(false)),
            sessions_dirty: Arc::new(AtomicBool::new(false)),
            read_health: crate::read_health::Health::default(),
            custody: SharedCustody::default(),
            binding: None,
            answer: resume::Answer::None,
        }
    }
}

impl Transport for Bridge {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        let result = self.require_rpc().and_then(|()| self.rpc.call(request));
        self.read_health.call(request, &result);
        result
    }
}

impl Bridge {
    /// Whether `attachment` is still this bridge's live attachment: an answer
    /// read on a replaced or closed one is never applied.
    /// The live attachment, as a command sent now would be fenced to it.
    #[cfg(test)]
    pub(crate) fn attachment(&self) -> Arc<AtomicBool> {
        self.fence.clone()
    }

    pub(crate) fn attachment_current(&self, attachment: &Arc<AtomicBool>) -> bool {
        Arc::ptr_eq(attachment, &self.fence) && self.fence.load(Ordering::Acquire)
    }

    pub(crate) fn read_peer(&self) -> Result<ReadPeer, TransportError> {
        self.require_rpc()?;
        Ok(ReadPeer {
            rpc: self.rpc.command_peer()?,
            valid: self.fence.clone(),
        })
    }

    pub(super) fn stop_peer(&self) -> Result<StopPeer, TransportError> {
        self.require_rpc()?;
        Ok(StopPeer {
            rpc: self.rpc.command_peer()?,
            valid: self.fence.clone(),
        })
    }

    /// The connection as a renderer sees it.
    pub fn connection(&self) -> ConnectionView {
        ConnectionView {
            state: self.state.clone(),
            last_event_age: self.last_event.map(|at| at.elapsed()),
        }
    }

    /// The reader revokes the fence the moment its stream closes, ahead of
    /// any frames still queued; a catalog hold can keep the Closed frame
    /// itself waiting. The revocation is the close, so record it now.
    fn observe_fence(&mut self) {
        if self.state.streaming() && !self.fence.load(Ordering::Acquire) {
            self.observe(Event::Closed);
        }
    }

    /// The backend reports each catalog refresh it attempts.
    pub(crate) fn catalog_refreshed(&mut self, succeeded: bool) {
        self.gate.refreshed(succeeded);
    }

    fn observe(&mut self, event: Event) {
        self.state = self.state.next(event, Instant::now());
    }

    /// A refused subscribe becomes a visible failure with its cause.
    fn report(&mut self, refusal: Option<TransportError>) {
        if let Some(refusal) = refusal {
            self.observe(Event::Failed {
                reason: refusal.to_string(),
            });
        }
    }

    /// Ask the reader for a replacement. A full request channel already holds
    /// one, so this joins it and lets `poll_reconnect` drain the old handoff
    /// that may be blocking the reader; only a gone reader is refused.
    fn request_replacement(&self) -> Result<(), TransportError> {
        match self.reconnect.try_send(()) {
            Ok(()) | Err(mpsc::TrySendError::Full(())) => Ok(()),
            Err(mpsc::TrySendError::Disconnected(())) => Err(TransportError::NotConnected),
        }
    }

    /// Multi-step reads and operator work wait for a pending reconnect: a
    /// replacement installed mid-operation would orphan its reply. One-shot
    /// calls (submit, stop) finish on the current attachment and stay allowed.
    fn require_incremental(&self) -> Result<(), TransportError> {
        self.require_rpc()?;
        if self.replacement_requested || !self.state.accepts_incremental() {
            return Err(TransportError::Unavailable {
                reason: "service reconnecting; draft kept".into(),
            });
        }
        Ok(())
    }

    fn require_rpc(&self) -> Result<(), TransportError> {
        // Commands depend on the attachment, never on live-view health: only a
        // closed or unverified attachment refuses them. The fence can revoke
        // a closed stream before its Closed frame is drained.
        if self.state.accepts_commands() && self.fence.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(TransportError::Unavailable {
                reason: "service reconnecting; draft kept".into(),
            })
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.fence.store(false, Ordering::Release);
    }
}

impl Connection for Bridge {
    fn watch_session(&mut self, session: Option<grapt_types::SessionId>) {
        // Record it for the reader first, then name it on the command peer.
        // The stream itself is owned by the reader thread, which reads this
        // shared name on every (re)subscription it opens.
        let changed = match self.session.lock() {
            Ok(mut held) => held.show(session),
            Err(poisoned) => poisoned.into_inner().show(session),
        };
        if changed {
            // The open stream still names the previous Session. Drop it so the
            // next subscribe registers the new one instead of leaving a stale
            // window on the service.
            self.stream_cancel = None;
            // A replacement may be mid-handshake for the Session just left. Its
            // barrier is still ahead in the handoff and would install that lease
            // when it lands, so advance the generation: `poll_reconnect` refuses
            // any barrier built under an older one and asks the reader again.
            self.generation.fetch_add(1, Ordering::AcqRel);
            self.session_changed();
        }
    }

    fn begin_reconnect(&mut self) -> Result<Option<bool>, TransportError> {
        if let Some(missing) = self.too_old {
            return Err(TransportError::ServerTooOld { missing });
        }
        self.observe_fence();
        if !self.replacement_requested {
            // The current attachment keeps serving commands until a verified
            // replacement is installed; a closed one was already invalidated.
            self.gate.reset();
            self.rpc.cancel_read();
            self.rpc.cancel_catalog();
            self.rpc.cancel_tool_read();
            self.rpc.cancel_operator();
            self.stream_cancel = None;
            self.request_replacement()?;
            self.replacement_requested = true;
            self.observe(Event::Replacing);
        }
        self.poll_reconnect()
    }
    fn poll_reconnect(&mut self) -> Result<Option<bool>, TransportError> {
        if let Some(missing) = self.too_old {
            return Err(TransportError::ServerTooOld { missing });
        }
        if !self.replacement_requested {
            return Ok(Some(self.subscribed()));
        }
        // Drain the old generation even when its reader is blocked sending a
        // full handoff's final Closed. The single reader places replacement
        // after all old frames and before every new frame. Stop at that barrier:
        // clearing the entire queue could discard the new generation's prefix.
        let mut replacement = None;
        for _ in 0..HANDOFF_FRAMES {
            match self.frames.try_recv() {
                Ok(Delivery::Frame(_)) => {}
                Ok(Delivery::Replacement(result, generation)) => {
                    replacement = Some((result, generation));
                    break;
                }
                // The reader abandoned a build for a Session the window had
                // already left. Nothing can be installed, so ask it to build
                // again for the Session selected now.
                Ok(Delivery::Stale) => {
                    self.observe(Event::Replacing);
                    self.request_replacement()?;
                    return Ok(None);
                }
                Err(mpsc::TryRecvError::Empty) => return Ok(None),
                Err(mpsc::TryRecvError::Disconnected) => return Err(TransportError::NotConnected),
            }
        }
        let Some((result, generation)) = replacement else {
            return Ok(None);
        };
        self.replacement_requested = false;
        // A barrier built before the last selection change names a Session this
        // window has left. Installing it would give the service that lease and
        // none for the Session now shown, so it is dropped and a fresh
        // replacement is requested instead. Its attachment is dropped with it,
        // which closes the socket that never became the live stream.
        if generation != self.generation.load(Ordering::Acquire) {
            self.observe(Event::Replacing);
            self.request_replacement()?;
            self.replacement_requested = true;
            return Ok(None);
        }
        let Attachment { rpc, cancel, valid } = match result {
            Ok(attachment) => *attachment,
            Err(refusal) => {
                // No verified replacement: the service may have restarted as a
                // different product, so the old attachment refuses commands too.
                self.fence.store(false, Ordering::Release);
                let reason = refusal.to_string();
                // A service older than this client refuses every retry the
                // same way, so it is refused like another product, never
                // retried as an outage, and keeps its typed refusal.
                let too_old = matches!(refusal, TransportError::ServerTooOld { .. });
                if let TransportError::ServerTooOld { missing } = refusal {
                    self.too_old = Some(missing);
                }
                self.observe(if too_old || unverified(&reason) {
                    Event::Unverified {
                        reason: reason.clone(),
                    }
                } else {
                    Event::Failed {
                        reason: reason.clone(),
                    }
                });
                return Err(if too_old {
                    refusal
                } else {
                    TransportError::Unavailable { reason }
                });
            }
        };
        // Copyright 2023-2026 SpaceXAI. Modified: install the verified profile
        // connections before allowing reads or effects; never replay commands.
        // Fence the old attachment so a detached stop cannot use it afterwards.
        self.fence.store(false, Ordering::Release);
        self.rpc = rpc;
        self.fence = valid;
        self.stream_cancel = cancel;
        // A new generation starts its own catalog hold.
        self.gate.reset();
        // A verified replacement of the current generation is the only thing
        // that ends a retirement.
        self.retired = false;
        self.observe(Event::Attached);
        // Raises the permission and session refreshes, unless this build
        // carried the resume offer: then they wait for the first frame.
        self.adopt_custody();
        Ok(Some(self.subscribed()))
    }
    fn begin_read(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        let result = self
            .require_incremental()
            .and_then(|()| self.rpc.begin_read(request));
        self.read_health.begin(0, request, &result);
        result
    }
    fn poll_read(&mut self) -> Result<Option<WireResponse>, TransportError> {
        let result = self.rpc.poll_read();
        self.read_health.poll(0, &result);
        result
    }
    fn cancel_read(&mut self) {
        self.read_health.cancel(0);
        self.rpc.cancel_read();
    }
    fn begin_catalog(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        let result = self
            .require_incremental()
            .and_then(|()| self.rpc.begin_catalog(request));
        self.read_health.begin(1, request, &result);
        result
    }
    fn poll_catalog(&mut self) -> Result<Option<WireResponse>, TransportError> {
        let result = self.rpc.poll_catalog();
        self.read_health.poll(1, &result);
        result
    }
    fn cancel_catalog(&mut self) {
        self.read_health.cancel(1);
        self.rpc.cancel_catalog();
    }

    fn begin_operator(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        self.read_health.write();
        self.require_incremental()?;
        self.rpc.begin_operator(request)
    }
    fn operator_human_wait(&mut self, waiting: bool) {
        self.rpc.operator_human_wait(waiting);
    }
    fn poll_operator(&mut self) -> Result<Option<WireResponse>, TransportError> {
        self.read_health.write();
        self.rpc.poll_operator()
    }
    fn cancel_operator(&mut self) {
        self.rpc.cancel_operator();
    }

    fn begin_tool_read(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        let result = self
            .require_incremental()
            .and_then(|()| self.rpc.begin_tool_read(request));
        self.read_health.begin(2, request, &result);
        result
    }
    fn poll_tool_read(&mut self) -> Result<Option<WireResponse>, TransportError> {
        let result = self.rpc.poll_tool_read();
        self.read_health.poll(2, &result);
        result
    }
    fn cancel_tool_read(&mut self) {
        self.read_health.cancel(2);
        self.rpc.cancel_tool_read();
    }
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        // The one delivery guard: nothing is consumed for a replacement in
        // flight or a retired stream; `poll_reconnect` owns that draining.
        if self.replacement_requested || self.retired {
            return Ok(FramePoll::Idle);
        }
        self.observe_fence();
        // Loss bypasses catalog gating and any queued live prefix. Stay live
        // until the common client sees it, so reconnect cannot discard the gap.
        if self.gaps.try_recv().is_ok() {
            self.gate.reset();
            self.observe(Event::Lost);
            return Ok(FramePoll::Gap);
        }
        // The reader raises this before enqueueing a new execution. Yield to
        // Backend's catalog refresh before the common client sees its activity;
        // unlisted sessions must not acquire observations or foreign views.
        // Check after receiving: the reader can raise the flag between an
        // earlier check and enqueueing. One retained handoff covers that race.
        let frame = match self.gate.take() {
            Some(frame) => Some(frame),
            None => match self.frames.try_recv() {
                Ok(Delivery::Frame(frame)) => Some(frame),
                Ok(Delivery::Replacement(..) | Delivery::Stale) => {
                    return Err(TransportError::NotConnected)
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    // A reader that ended has closed its stream; there is no
                    // frame left to deliver, only the state to record.
                    if self.state.streaming() {
                        self.observe(Event::Closed);
                    }
                    None
                }
                Err(mpsc::TryRecvError::Empty) => None,
            },
        };
        // The first frame of an offered build answers the resume, so it is
        // never held behind a catalog refresh the answer itself releases.
        let dirty = self.sessions_dirty.load(Ordering::Acquire)
            && self.answer != resume::Answer::FirstFrame;
        let frame = match self.gate.admit(frame, dirty) {
            Gate::Pass(frame) => frame.unwrap_or(FramePoll::Idle),
            Gate::Hold => return Ok(FramePoll::Idle),
            Gate::Resync => {
                self.observe(Event::Lost);
                return Ok(FramePoll::Gap);
            }
        };
        // Ordered terminal frames also retire fixture or replacement streams.
        match frame {
            FramePoll::Closed => self.observe(Event::Closed),
            FramePoll::Gap => self.observe(Event::Lost),
            FramePoll::Frame(_) | FramePoll::Positioned { .. } => {
                self.last_event = Some(Instant::now());
            }
            _ => {}
        }
        if frame != FramePoll::Idle && self.answer == resume::Answer::FirstFrame {
            self.answer = resume::Answer::Delivered;
        }
        if matches!(frame, FramePoll::Closed | FramePoll::Gap) {
            self.binding = None;
        }
        Ok(frame)
    }
    fn resubscribe(&mut self) {
        if !self.subscribed() {
            let _ = self.begin_reconnect();
        }
    }
    fn retire_stream(&mut self) {
        // The retired attachment never reads live again. Any barrier built
        // before now is stale; a fresh verified replacement may install at once.
        self.retired = true;
        self.generation.fetch_add(1, Ordering::AcqRel);
        if self.begin_reconnect().is_err() {
            // The reader is gone: the attachment is closed, and stays retired.
            self.fence.store(false, Ordering::Release);
            self.observe_fence();
        }
    }
    fn refuse_stream(&mut self, missing: &'static str) {
        // Retired like a contract break, but terminally: no replacement is
        // requested now or later, because the same service refuses each one.
        self.too_old = Some(missing);
        self.retired = true;
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.stream_cancel = None;
        self.fence.store(false, Ordering::Release);
        self.observe_fence();
        let reason = TransportError::ServerTooOld { missing }.to_string();
        self.observe(Event::Unverified { reason });
    }
    fn subscribed(&self) -> bool {
        // A replacement counts only once poll_reconnect installs it, and a
        // revoked fence is a closed stream even before its frame is drained.
        !self.replacement_requested && self.state.streaming() && self.fence.load(Ordering::Acquire)
    }
    fn stream_binding(&self) -> Option<grapt_client::client::StreamBinding> {
        self.bound_stream()
    }
    fn resume_from(&mut self, token: Option<grapt_client::client::ResumeToken>) {
        self.offer(token);
    }
    fn resume_suffix(&mut self, frames: usize) {
        self.offer_suffix(frames);
    }
    fn resume_answered(&mut self, certified: bool) {
        self.answered(certified);
    }
}

/// A replacement that answered as another product, or could not prove it was
/// this one, is refused rather than retried as an ordinary outage.
fn unverified(reason: &str) -> bool {
    [
        "differs from the launcher-selected product",
        "incompatible readiness receipt",
        "product verification failed",
    ]
    .iter()
    .any(|marker| reason.contains(marker))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tool_read_tests;

#[cfg(test)]
mod restart_tests;

#[cfg(test)]
mod replacement_tests;

#[cfg(test)]
mod read_peer_tests;
