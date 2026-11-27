//! Authenticated versioned capture: a bounded durable replay that joins the
//! live fan-out without a gap and ends with a typed terminal frame.
//!
//! J03 of EVENT-STREAM-REPLAY-CURSOR-01. The v2 `/api/event` route is
//! untouched: this is a separate registration over the same hub, reusing the
//! sink fan-out and the frame codec only.
//!
//! Law summary (full contract in `docs/event-stream-replay-cursor/CONTRACT.md`):
//! - Auth first: no sealed principal means no port call and no stream.
//! - Interest: `all`, or a location naming a directory and an optional
//!   `wrk_*` workspace; the interest is sealed into the cursor token.
//! - Cursor: absent means start; present must decode under THIS interest.
//! - The capture sink is registered BEFORE the first durable read, so no
//!   committed frame can fall between the last replay page and the live join.
//! - Wake-driven: a live frame only re-enters the durable replay from the
//!   fence; the page is the sole presentation source, so a committed row
//!   with no v2 frame still reaches the reader exactly once.
//! - Exactly once per (aggregate, seq): the shared fence is the single
//!   authoritative gate in the body; the sink filter only protects the queue.
//! - Bounded everything: pages, queues, coordinates, tokens and frames.
//! - Honest failure: every end is a typed `capture.terminated` frame, then
//!   EOF; absent is not unreadable, unreadable is not corrupt.

use std::collections::BTreeMap;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
#[cfg(test)]
use axum::http::header::CONTENT_TYPE;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use grapt_schema::{OptionalNullable, Required};
use hyper::body::Frame;
use tokio::sync::mpsc;

use crate::{ApiRefusal, ApiRouterBuilder, RouteMethod, RoutePath, SealedPrincipal};

use super::application::{ApplicationFuture, ApplicationUnavailable, CaptureApplicationPort};
use super::capture_wire::{
    capture_query, coordinate_dto, ConnectedFrame, CursorFrame, InterestEcho, ReplayEventFrame,
    SkippedFrame, TerminatedFrame,
};
use super::events::DogfoodEvent;
use super::routes::{invalid_request_response, json, unauthorized_response, unavailable_response};
use super::sse::{encode_sse, sse_response, DogfoodEventHub, SseLimits};
use super::{ConflictErrorDto, ConflictErrorDtoTag, DtoString};
use grapt_types::{
    CaptureCoordinate, CaptureCursor, CaptureCursorError, CaptureInterest, EventFencing, EventSeq,
    SessionId, MAX_CAPTURE_CURSOR_COORDINATES,
};

/// The new authenticated capture stream. Additive: it does not enter the
/// frozen non-SSE route inventory and is not described by the frozen OpenAPI
/// document, which stays byte-identical.
pub const CAPTURE_EVENT_PATH: &str = "/api/capture/v1/events";

// ---------------------------------------------------------------------------
// The application seam: one bounded page of durable truth under one
// interest-bound cursor. The API crate owns no store; the product implements
// this over the store's governed read.
// ---------------------------------------------------------------------------

/// One bounded page of durable capture truth.
#[derive(Debug, Clone)]
pub struct CapturePage {
    pub rows: Vec<CapturePageRow>,
    /// Where the reader continues: `None` ends the replay and the stream
    /// crosses over to the live join.
    pub continuation: Option<CaptureCursor>,
}

/// One durable row of the capture feed, with its typed coordinate.
#[derive(Debug, Clone)]
pub enum CapturePageRow {
    Event {
        coordinate: CaptureCoordinate,
        event: Box<DogfoodEvent>,
    },
    /// A durable row whose kind this reader does not know. The coordinate
    /// still advances the fence: skipping is honest, unbounded is not.
    Skipped {
        coordinate: CaptureCoordinate,
        kind: DtoString,
    },
}

impl CapturePageRow {
    pub fn coordinate(&self) -> &CaptureCoordinate {
        match self {
            Self::Event { coordinate, .. } | Self::Skipped { coordinate, .. } => coordinate,
        }
    }
}

/// A refused capture read, named by the failure class so the stream can end
/// honestly instead of guessing.
#[derive(Debug)]
pub enum CapturePageError {
    /// The cursor refused the read: malformed token, wrong version or
    /// location, or a coordinate the store cannot honestly serve.
    Cursor(CaptureCursorError),
    /// A row the read posture cannot decode; the whole page is refused.
    CorruptRow { id: DtoString, detail: DtoString },
    /// The store itself could not be read.
    UnreadableStore { detail: DtoString },
    /// The interest's durable aggregate span is wider than the cursor's
    /// coordinate bound: no single resume token can serve it. The width
    /// probe refuses this up front; a stream never presents it.
    InterestExceedsBound { width: u64 },
}

impl From<CaptureCursorError> for CapturePageError {
    fn from(error: CaptureCursorError) -> Self {
        Self::Cursor(error)
    }
}

impl std::fmt::Display for CapturePageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cursor(error) => formatter.write_str(&error.to_string()),
            Self::CorruptRow { id, detail } => {
                write!(
                    formatter,
                    "capture row {} is corrupt: {}",
                    id.as_str(),
                    detail.as_str()
                )
            }
            Self::UnreadableStore { detail } => {
                write!(
                    formatter,
                    "capture store is unreadable: {}",
                    detail.as_str()
                )
            }
            Self::InterestExceedsBound { width } => {
                write!(
                    formatter,
                    "capture interest spans {width} aggregates, past the cursor bound of {MAX_CAPTURE_CURSOR_COORDINATES}"
                )
            }
        }
    }
}

impl std::error::Error for CapturePageError {}

/// How a capture stream ends. Every end is a typed wire frame, never a bare
/// connection close: the client must learn the reason and its final cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureStreamTerminal {
    /// A live frame arrived with no queue room; resume from the final cursor.
    Overflow,
    /// A failed projection closed the advisory stream; reconnect and hydrate
    /// durable truth.
    Invalidated,
    /// A committed reconciliation boundary ended the stream; queued frames
    /// remain valid and drain before this frame.
    Reconciled,
    /// A replay page refused a corrupt row.
    CorruptRow,
    /// A replay page could not read the store.
    UnreadableStore,
    /// The fence reached the cursor's coordinate cap before the row could be
    /// presented; no honest resume token exists beyond it.
    CoordinateBound,
    /// A replay row did not fit one SSE frame; the fence did not advance,
    /// so a reconnect re-lists the row from the final cursor.
    FrameBound,
    /// The port's continuation was refused by the cursor law; the stream
    /// ends on the last honest token, naming the lying port.
    CursorRefused,
}

impl CaptureStreamTerminal {
    pub fn wire_reason(self) -> &'static str {
        match self {
            Self::Overflow => "overflow",
            Self::Invalidated => "invalidated",
            Self::Reconciled => "reconciled",
            Self::CorruptRow => "corrupt-row",
            Self::UnreadableStore => "unreadable-store",
            Self::CoordinateBound => "coordinate-bound",
            Self::FrameBound => "frame-bound",
            Self::CursorRefused => "cursor-refused",
        }
    }
}

// ---------------------------------------------------------------------------
// The shared live state: one honest view of "what this stream has presented".
// ---------------------------------------------------------------------------

/// The result of trying to present one coordinate to the fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PresentOutcome {
    Presented,
    AlreadyHeld,
    CoordinateBound,
}

/// The per-stream fence and first-wins terminal, shared by the hub's capture
/// sink (queue protection) and the stream body (the authoritative
/// exactly-once gate). The fence only moves forward; a poisoned lock hands
/// back its guard rather than dying in the stream.
pub(super) struct CaptureLiveState {
    fence: Mutex<BTreeMap<SessionId, EventSeq>>,
    terminal: Mutex<Option<CaptureStreamTerminal>>,
}

impl std::fmt::Debug for CaptureLiveState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CaptureLiveState")
            .field("fence", &self.lock_fence().len())
            .field("terminal", &self.terminal())
            .finish()
    }
}

impl CaptureLiveState {
    pub(super) fn new() -> Self {
        Self {
            fence: Mutex::new(BTreeMap::new()),
            terminal: Mutex::new(None),
        }
    }

    /// Seed the fence from the start cursor. Max-only advance: seeding never
    /// moves a held sequence backward.
    pub(super) fn seed(&self, start: &CaptureCursor) {
        let mut fence = self.lock_fence();
        for coordinate in start.coordinates() {
            if let Some(held) = fence.get_mut(&coordinate.aggregate) {
                if *held < coordinate.seq {
                    *held = coordinate.seq;
                }
            } else {
                fence.insert(coordinate.aggregate.clone(), coordinate.seq);
            }
        }
    }

    /// Whether the fence has already served this aggregate at or beyond this
    /// sequence.
    pub(super) fn holds(&self, aggregate: &SessionId, seq: EventSeq) -> bool {
        fence_holds(&self.lock_fence(), aggregate, seq)
    }

    /// Present one coordinate. A duplicate is fenced, a new aggregate past
    /// the cursor's coordinate cap is refused before it poisons the fence,
    /// and anything else advances the fence and is presented.
    pub(super) fn try_present(&self, aggregate: &SessionId, seq: EventSeq) -> PresentOutcome {
        let mut fence = self.lock_fence();
        if fence_holds(&fence, aggregate, seq) {
            return PresentOutcome::AlreadyHeld;
        }
        if fence.get(aggregate).is_none() && fence.len() >= MAX_CAPTURE_CURSOR_COORDINATES {
            return PresentOutcome::CoordinateBound;
        }
        fence.insert(aggregate.clone(), seq);
        PresentOutcome::Presented
    }

    /// Arm the stream terminal; the first arm wins and later arms are inert.
    pub(super) fn arm_terminal(&self, terminal: CaptureStreamTerminal) {
        let mut held = self
            .terminal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if held.is_none() {
            *held = Some(terminal);
        }
    }

    pub(super) fn terminal(&self) -> Option<CaptureStreamTerminal> {
        *self
            .terminal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Mint the honest resume token from the fence: every held aggregate at
    /// its held sequence. Live and replay rows carry no writer epoch on this
    /// surface, so the minted coordinates are `LegacyUnfenced` — the store's
    /// read excludes by sequence only, so the fencing is inert there.
    pub(super) fn mint_cursor(&self, interest: &CaptureInterest) -> Option<CaptureCursor> {
        let fence = self.lock_fence();
        let coordinates = fence
            .iter()
            .map(|(aggregate, seq)| CaptureCoordinate {
                aggregate: aggregate.clone(),
                seq: *seq,
                fencing: EventFencing::LegacyUnfenced,
            })
            .collect();
        CaptureCursor::new(interest.clone(), coordinates).ok()
    }

    fn lock_fence(&self) -> MutexGuard<'_, BTreeMap<SessionId, EventSeq>> {
        self.fence
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The exactly-once comparison: the held value is the last sequence presented
/// for its aggregate, so any sequence at or below it is already served. EVR3
/// pins this one line; mutating it resurrects duplicates on replay and on the
/// live join.
fn fence_holds(
    fence: &BTreeMap<SessionId, EventSeq>,
    aggregate: &SessionId,
    seq: EventSeq,
) -> bool {
    fence
        .get(aggregate)
        .is_some_and(|held_seq| *held_seq >= seq)
}

/// Resolve the stream's start position: an absent cursor is the start of the
/// interest; a present one must decode under THIS interest.
fn resolve_cursor(
    raw: Option<DtoString>,
    interest: &CaptureInterest,
) -> Result<CaptureCursor, CaptureCursorError> {
    match raw {
        None => Ok(CaptureCursor::start(interest.clone())),
        Some(token) => CaptureCursor::decode(token.as_str(), interest),
    }
}

// ---------------------------------------------------------------------------
// Pre-stream typed refusals: 401 auth, 400 query and cursor grammar, 409
// cursor law. A refusal is a complete answer: no stream, and no port call
// after the first page read it names.
// ---------------------------------------------------------------------------

fn capture_cursor_refusal(error: &CaptureCursorError) -> Response {
    match error {
        CaptureCursorError::Malformed { .. } | CaptureCursorError::WrongVersion { .. } => {
            invalid_request_response("invalid capture cursor")
        }
        CaptureCursorError::WrongLocation
        | CaptureCursorError::FutureCoordinate { .. }
        | CaptureCursorError::Stale { .. } => conflict_response("capture cursor refused"),
    }
}

fn conflict_response(message: &'static str) -> Response {
    let Some(message) = DtoString::try_new(message).ok() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    json(
        StatusCode::CONFLICT,
        &ConflictErrorDto::new(
            Required::new(ConflictErrorDtoTag::ConflictError),
            Required::new(message),
            OptionalNullable::absent(),
        ),
    )
}

fn capture_feed_unavailable() -> Response {
    unavailable_response(ApplicationUnavailable::try_new("capture feed"))
}

/// The up-front width refusal: the interest's aggregate span exceeds the
/// cursor's coordinate bound, so no resume token could ever serve it.
/// Distinct from a cursor-law refusal: the documented client action is to
/// narrow the interest, not to resume from a token.
fn capture_width_refusal() -> Response {
    conflict_response("capture interest exceeds the cursor bound")
}

/// The capture protocol version this surface serves.
pub const CAPTURE_PROTOCOL_VERSION: u64 = 1;

/// The stream mints an explicit cursor token frame every 64 presented
/// coordinates; the final token always rides the terminal frame.
const CURSOR_TOKEN_EVERY_ROWS: usize = 64;

// ---------------------------------------------------------------------------
// The stream body: REPLAY crosses into LIVE through one shared fence.
// ---------------------------------------------------------------------------

/// One outcome of a body step.
enum Step {
    /// Present this frame now.
    Frame(Bytes),
    /// Wait for the in-flight page or the live queue.
    Pending,
    /// Produce nothing; step again.
    Continue,
}

enum CaptureState {
    /// Emit the connected frame, then the already-fetched first page.
    Connected {
        first_page: Option<CapturePage>,
    },
    Replay {
        /// The page being presented, row by row.
        page: Option<CapturePage>,
        row: usize,
        /// The continuation to serve once the in-hand page is exhausted.
        next: Option<CaptureCursor>,
        /// The in-flight continuation page, once requested.
        pending: Option<ApplicationFuture<CapturePage, CapturePageError>>,
    },
    Live,
    Done,
}

/// The capture stream. The fence is the single exactly-once gate: every
/// presented coordinate advances it, every fenced coordinate is dropped,
/// and every end is a typed capture.terminated frame.
pub struct CaptureBody {
    application: Arc<dyn CaptureApplicationPort>,
    principal: SealedPrincipal,
    interest: CaptureInterest,
    live: Arc<CaptureLiveState>,
    receiver: mpsc::Receiver<Bytes>,
    overflowed: Arc<AtomicBool>,
    sse_limits: SseLimits,
    /// The token the stream opened with: the fallback if the fence could
    /// not mint an honest token (it always can; the fence is bounded at
    /// the coordinate cap).
    start_token: String,
    /// A frame already produced, awaiting its poll.
    queued_frame: Option<Bytes>,
    presented: usize,
    state: CaptureState,
}

impl core::fmt::Debug for CaptureBody {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("CaptureBody")
            .field("overflowed", &self.overflowed.load(Ordering::Acquire))
            .field("presented", &self.presented)
            .field("done", &matches!(self.state, CaptureState::Done))
            .finish_non_exhaustive()
    }
}
/// The result of presenting one replay row through the fence.
enum PresentStep {
    /// The fence advanced; the encoded frame is the next wire frame.
    Presented(Step),
    /// The fence already held the coordinate; nothing is presented.
    Fenced,
    /// The stream ended with a typed terminal frame.
    Terminated(Step),
}

/// One poll advances at most one page (64 rows) plus a bounded number of
/// state transitions. A poll that exhausts this bound has broken the state
/// machine and refuses loudly instead of looping.
const MAX_POLL_STEPS: usize = 256;

/// A mid-stream port refusal, named for its failure class. The real store
/// refuses a continuation only when the cursor outlives its truth; anything
/// else means the port lied, and the stream ends saying exactly that.
fn page_refusal_terminal(refusal: &CapturePageError) -> CaptureStreamTerminal {
    match refusal {
        CapturePageError::Cursor(_) => CaptureStreamTerminal::CursorRefused,
        CapturePageError::CorruptRow { .. } => CaptureStreamTerminal::CorruptRow,
        CapturePageError::InterestExceedsBound { .. } => CaptureStreamTerminal::CursorRefused,
        CapturePageError::UnreadableStore { .. } => CaptureStreamTerminal::UnreadableStore,
    }
}

impl CaptureBody {
    fn connected_frame(&self) -> Result<Bytes, io::Error> {
        encode_sse(
            &ConnectedFrame {
                frame_type: "capture.connected",
                version: CAPTURE_PROTOCOL_VERSION,
                interest: InterestEcho(self.interest.clone()),
                cursor: self.start_token.clone(),
            },
            self.sse_limits,
        )
        .map_err(|_| io::Error::other("capture connected frame exceeds the SSE frame bound"))
    }

    /// The honest resume token: minted from the fence, falling back to the
    /// token the stream opened with if the fence cannot mint one.
    fn mint_token(&self) -> String {
        match self
            .live
            .mint_cursor(&self.interest)
            .map(|cursor| cursor.encode())
        {
            Some(token) => token,
            None => self.start_token.clone(),
        }
    }

    /// The final wire frame: the armed terminal reason and the minted resume
    /// token, then EOF. The first armed terminal wins; an unexplained EOF
    /// names itself Invalidated rather than going silent.
    fn terminated_frame(&mut self) -> Result<Step, io::Error> {
        let terminal = match self.live.terminal() {
            Some(terminal) => terminal,
            None => CaptureStreamTerminal::Invalidated,
        };
        let frame = encode_sse(
            &TerminatedFrame {
                frame_type: "capture.terminated",
                reason: terminal.wire_reason(),
                cursor: self.mint_token(),
            },
            self.sse_limits,
        )
        .map_err(|_| io::Error::other("capture terminal frame exceeds the SSE frame bound"))?;
        self.state = CaptureState::Done;
        Ok(Step::Frame(frame))
    }

    /// Every 64 presented coordinates the stream mints an explicit cursor
    /// token frame, returned on the next poll through the queued slot.
    fn queue_cursor_token(&mut self) -> Result<(), io::Error> {
        let frame = encode_sse(
            &CursorFrame {
                frame_type: "capture.cursor",
                cursor: self.mint_token(),
            },
            self.sse_limits,
        )
        .map_err(|_| io::Error::other("capture cursor frame exceeds the SSE frame bound"))?;
        self.queued_frame = Some(frame);
        Ok(())
    }

    /// Present one replay row. The frame is encoded BEFORE the fence
    /// advances: a row that cannot fit one SSE frame ends the stream as
    /// FrameBound without moving the fence, so a reconnect re-lists it.
    fn present_row(&mut self, row: CapturePageRow) -> Result<PresentStep, io::Error> {
        let coordinate = row.coordinate().clone();
        let encoded = match &row {
            CapturePageRow::Event { coordinate, event } => encode_sse(
                &ReplayEventFrame {
                    frame_type: "capture.event",
                    coordinate: coordinate_dto(coordinate),
                    event,
                },
                self.sse_limits,
            ),
            CapturePageRow::Skipped { coordinate, kind } => encode_sse(
                &SkippedFrame {
                    frame_type: "capture.skipped",
                    coordinate: coordinate_dto(coordinate),
                    kind: kind.clone(),
                },
                self.sse_limits,
            ),
        };
        let frame = match encoded {
            Ok(frame) => frame,
            Err(_) => {
                self.live.arm_terminal(CaptureStreamTerminal::FrameBound);
                return Ok(PresentStep::Terminated(self.terminated_frame()?));
            }
        };
        match self.live.try_present(&coordinate.aggregate, coordinate.seq) {
            PresentOutcome::Presented => {
                self.presented += 1;
                if self.presented.is_multiple_of(CURSOR_TOKEN_EVERY_ROWS) {
                    self.queue_cursor_token()?;
                }
                Ok(PresentStep::Presented(Step::Frame(frame)))
            }
            PresentOutcome::AlreadyHeld => Ok(PresentStep::Fenced),
            PresentOutcome::CoordinateBound => {
                self.live
                    .arm_terminal(CaptureStreamTerminal::CoordinateBound);
                Ok(PresentStep::Terminated(self.terminated_frame()?))
            }
        }
    }

    /// One replay step: serve the next row, poll the in-flight page, request
    /// the next page, or cross over to the live join.
    fn replay_once(
        &mut self,
        page: Option<CapturePage>,
        row: usize,
        mut next: Option<CaptureCursor>,
        pending: Option<ApplicationFuture<CapturePage, CapturePageError>>,
        context: &mut Context<'_>,
    ) -> Result<Step, io::Error> {
        let mut pending = pending;
        if pending.is_some() {
            let polled = pending.as_mut().map(|future| future.as_mut().poll(context));
            if let Some(Poll::Ready(result)) = polled {
                let _completed = pending.take();
                match result {
                    Ok(completed) => {
                        let next = completed.continuation.clone();
                        self.state = CaptureState::Replay {
                            page: Some(completed),
                            row: 0,
                            next,
                            pending: None,
                        };
                        return Ok(Step::Continue);
                    }
                    Err(refusal) => {
                        self.live.arm_terminal(page_refusal_terminal(&refusal));
                        return self.terminated_frame();
                    }
                }
            }
            self.state = CaptureState::Replay {
                page,
                row,
                next,
                pending,
            };
            return Ok(Step::Pending);
        }
        if page.is_some() {
            if let Some(current) = page.as_ref().and_then(|page| page.rows.get(row)).cloned() {
                let row = row + 1;
                return match self.present_row(current)? {
                    PresentStep::Presented(frame) => {
                        self.state = CaptureState::Replay {
                            page,
                            row,
                            next,
                            pending,
                        };
                        Ok(frame)
                    }
                    PresentStep::Fenced => {
                        self.state = CaptureState::Replay {
                            page,
                            row,
                            next,
                            pending,
                        };
                        Ok(Step::Continue)
                    }
                    PresentStep::Terminated(frame) => Ok(frame),
                };
            }
        }
        match next.take() {
            Some(cursor) => {
                pending = Some(self.application.capture_page(
                    self.principal.clone(),
                    self.interest.clone(),
                    cursor,
                ));
                self.state = CaptureState::Replay {
                    page: None,
                    row: 0,
                    next: None,
                    pending,
                };
                Ok(Step::Continue)
            }
            None => {
                // The durable replay is exhausted: cross over to the live
                // join. The sink was registered before the first page, so
                // nothing committed since can fall between the two.
                self.state = CaptureState::Live;
                Ok(Step::Continue)
            }
        }
    }

    /// One live step. A queued wake means the durable truth may have moved:
    /// re-enter the replay from the fence and serve what the page owes.
    /// The hub's frame bytes are consumed, never presented — the durable
    /// page is the sole presentation source, so a committed row with no v2
    /// frame still reaches the reader exactly once. The sink already
    /// location-filtered and bounded the frame; the fence stays the
    /// authoritative exactly-once gate for the rows the page hands back.
    fn step_live(&mut self, context: &mut Context<'_>) -> Result<Step, io::Error> {
        match self.receiver.poll_recv(context) {
            Poll::Ready(Some(_wake)) => self.replay_from_fence(),
            Poll::Ready(None) => {
                // EOF with no armed terminal: the hub dropped out from under
                // the stream. Name it rather than going silent.
                self.live.arm_terminal(CaptureStreamTerminal::Invalidated);
                self.terminated_frame()
            }
            Poll::Pending => {
                if matches!(
                    self.live.terminal(),
                    Some(CaptureStreamTerminal::Reconciled)
                ) {
                    // The reconcile boundary armed and the queue drained:
                    // the close is now owed.
                    self.terminated_frame()
                } else {
                    Ok(Step::Pending)
                }
            }
        }
    }

    /// Re-enter the durable replay from the fence after a live wake: the
    /// minted token names exactly what the fence holds, and the page from
    /// it is the owed truth — the rows the fence passed, in store order.
    fn replay_from_fence(&mut self) -> Result<Step, io::Error> {
        let cursor = match self.live.mint_cursor(&self.interest) {
            Some(cursor) => cursor,
            None => {
                // The fence cannot mint an honest token: the cursor law
                // refuses the resume this stream would owe. End naming it.
                self.live.arm_terminal(CaptureStreamTerminal::CursorRefused);
                return self.terminated_frame();
            }
        };
        self.state = CaptureState::Replay {
            page: None,
            row: 0,
            next: Some(cursor),
            pending: None,
        };
        Ok(Step::Continue)
    }

    /// One state-machine step. The fatal guards run first: an overflowed
    /// latch, or an armed fatal terminal, ends the stream on the next
    /// boundary; Reconciled is the only terminal that still owes a drain.
    fn step(&mut self, context: &mut Context<'_>) -> Result<Step, io::Error> {
        if self.overflowed.load(Ordering::Acquire) {
            self.live.arm_terminal(CaptureStreamTerminal::Overflow);
            return self.terminated_frame();
        }
        if let Some(terminal) = self.live.terminal() {
            if !matches!(terminal, CaptureStreamTerminal::Reconciled) {
                return self.terminated_frame();
            }
        }
        match &mut self.state {
            CaptureState::Connected { first_page } => {
                let first_page = first_page.take();
                let next = first_page
                    .as_ref()
                    .and_then(|page| page.continuation.clone());
                let frame = self.connected_frame()?;
                self.state = CaptureState::Replay {
                    page: first_page,
                    row: 0,
                    next,
                    pending: None,
                };
                Ok(Step::Frame(frame))
            }
            CaptureState::Replay { .. } => {
                let (page, row, next, pending) =
                    match core::mem::replace(&mut self.state, CaptureState::Done) {
                        CaptureState::Replay {
                            page,
                            row,
                            next,
                            pending,
                        } => (page, row, next, pending),
                        other => {
                            self.state = other;
                            return Ok(Step::Continue);
                        }
                    };
                self.replay_once(page, row, next, pending, context)
            }
            CaptureState::Live => self.step_live(context),
            CaptureState::Done => Ok(Step::Continue),
        }
    }
}

impl hyper::body::Body for CaptureBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if let Some(frame) = this.queued_frame.take() {
            return Poll::Ready(Some(Ok(Frame::data(frame))));
        }
        if matches!(this.state, CaptureState::Done) {
            return Poll::Ready(None);
        }
        for _ in 0..MAX_POLL_STEPS {
            match this.step(context) {
                Ok(Step::Frame(frame)) => return Poll::Ready(Some(Ok(Frame::data(frame)))),
                Ok(Step::Pending) => return Poll::Pending,
                Ok(Step::Continue) => {}
                Err(error) => return Poll::Ready(Some(Err(error))),
            }
        }
        Poll::Ready(Some(Err(io::Error::other(
            "capture stream exceeded its per-poll step bound",
        ))))
    }
}

// ---------------------------------------------------------------------------
// The route: pre-stream typed refusals, then the 200 SSE answer.
// ---------------------------------------------------------------------------

/// The capture stream handler. The order is the law: authentication refuses
/// before any query is parsed, the cursor law refuses before any stream
/// exists, the width probe refuses an over-wide interest before any sink
/// exists, the sink is registered BEFORE the first durable read, and the
/// first page is awaited pre-stream so its refusals answer as HTTP statuses
/// rather than as terminal frames.
pub(super) async fn capture_event_handler(
    request: Request<Body>,
    hub: Arc<DogfoodEventHub>,
    application: Arc<dyn CaptureApplicationPort>,
) -> Response {
    let Some(principal) = request.extensions().get::<SealedPrincipal>().cloned() else {
        return unauthorized_response();
    };
    let query = match capture_query(request.uri().query()) {
        Ok(query) => query,
        Err(_) => return invalid_request_response("invalid capture query"),
    };
    let start_cursor = match resolve_cursor(query.cursor, &query.interest) {
        Ok(cursor) => cursor,
        Err(error) => return capture_cursor_refusal(&error),
    };
    let width = match application
        .count_capture_aggregates(principal.clone(), query.interest.clone())
        .await
    {
        Ok(width) => width,
        Err(CapturePageError::Cursor(error)) => return capture_cursor_refusal(&error),
        Err(CapturePageError::InterestExceedsBound { .. }) => return capture_width_refusal(),
        Err(_) => return capture_feed_unavailable(),
    };
    if width > MAX_CAPTURE_CURSOR_COORDINATES as u64 {
        return capture_width_refusal();
    }
    let live = Arc::new(CaptureLiveState::new());
    live.seed(&start_cursor);
    let subscription = match hub.subscribe_capture(query.interest.clone(), Arc::clone(&live)) {
        Ok(subscription) => subscription,
        Err(_) => return capture_feed_unavailable(),
    };
    let first_page = match application
        .capture_page(
            principal.clone(),
            query.interest.clone(),
            start_cursor.clone(),
        )
        .await
    {
        Ok(page) => page,
        Err(CapturePageError::Cursor(error)) => return capture_cursor_refusal(&error),
        Err(_) => return capture_feed_unavailable(),
    };
    let sse_limits = hub.limits();
    sse_response(CaptureBody {
        application,
        principal,
        interest: query.interest,
        live,
        receiver: subscription.receiver,
        overflowed: subscription.overflowed,
        sse_limits,
        start_token: start_cursor.encode(),
        queued_frame: None,
        presented: 0,
        state: CaptureState::Connected {
            first_page: Some(first_page),
        },
    })
}

/// Registers the authenticated capture stream. Additive: the frozen v2
/// route, its bytes and its OpenAPI document are untouched; a duplicate
/// registration refuses like any other route collision.
pub fn register_capture_event_route(
    builder: &mut ApiRouterBuilder,
    hub: Arc<DogfoodEventHub>,
    application: Arc<dyn CaptureApplicationPort>,
) -> Result<(), ApiRefusal> {
    builder.register(
        RouteMethod::Get,
        RoutePath::try_new(CAPTURE_EVENT_PATH)?,
        move |request: Request<Body>| {
            let hub = Arc::clone(&hub);
            let application = Arc::clone(&application);
            async move { capture_event_handler(request, hub, application).await }
        },
    )
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
