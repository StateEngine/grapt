//! The typed transcript projection: a pure fold over admitted events.
//!
//! ## Why this is a fold and not a widget tree
//!
//! The god-file map names `tui/src/routes/session/index.tsx` (3,480 LOC) as
//! DO-NOT-PORT and directs "a separate typed projection/reducer, session
//! controller and small widgets". This module is the projection/reducer half,
//! and it is deliberately a pure function: `apply` takes `&mut self` and an
//! event and returns a typed outcome, touching no I/O, no clock and no
//! terminal.
//!
//! The payoff is that every law below can be witnessed against a synthetic
//! transcript carrying the exact defect it exists to catch — including
//! transcripts no server can be made to produce on demand: a delta with no
//! opening block, a second terminal for one execution, a stream that ends
//! mid-slice. Those are the interleavings that break a renderer written
//! against the happy path, and none of them can be scheduled against a live
//! engine.
//!
//! ## Bounds are visible, never silent
//!
//! Every bound in [`crate::bounds`] that this module enforces also records
//! what it dropped. A transcript that quietly discarded its oldest slices
//! would show the user a complete-looking history that is missing content, and
//! nothing on screen would say so. So [`SessionView::dropped_slices`] and
//! [`Slice::truncated_bytes`] are part of the rendered model, not internal
//! bookkeeping.

use crate::bounds::Bounds;
use crate::contract::{EventEnvelope, EventMember};

use thiserror::Error;

use crate::wire::Spelled;
mod cell;
mod diff;
mod exec;
pub mod feedback;
mod footer;
mod offset;
mod plan;
mod progress;
mod proof;
mod provisional;
mod quiescence;
mod stamp;
pub(crate) use quiescence::LiveQuiescence;
pub use stamp::{supersedes, FactStamp};
mod read;
mod tool_row;
mod tool_summary;
pub use tool_summary::{StdinAction, ToolSummary};
mod window;

pub use cell::{Slice, SliceKind, ToolPhase};
pub use diff::Mark;

/// The lifecycle of one execution, as the wire describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionState {
    /// No execution has started, or the last one settled.
    Idle,
    /// An execution is running.
    Running {
        execution_id: crate::wire::ExecutionWireId,
    },
    /// An execution reached exactly one terminal.
    ///
    /// `execution_id` is `None` exactly when the terminal came from the DURABLE
    /// re-list rather than from a live `session.execution.*` frame. The re-list
    /// records how an assistant message finished, and it names no execution —
    /// so there is no execution id to carry, and the previous revision
    /// invented the literal string `"durable"` to fill the field.
    ///
    /// That invention is why this is an `Option` rather than a typed id with a
    /// sentinel value: `"durable"` parses as a perfectly valid opaque
    /// `ExecutionId`, so nothing would ever have refused it, and a terminal
    /// attributed to a nonexistent execution compares unequal to every real
    /// one. Absence is the honest encoding of an absent fact.
    Settled {
        execution_id: Option<crate::wire::ExecutionWireId>,
        outcome: Outcome,
    },
}

/// The terminal vocabulary, owned by the contract layer.
pub use crate::contract::Outcome;

/// Token accounting as last reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    pub input: Option<u64>,
    pub output: Option<u64>,
}

/// The rendered model for one session.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionView {
    pub session_id: crate::wire::SessionWireId,
    pub queued: crate::wire::QueuedInputPreview,
    pub slices: Vec<Slice>,
    pub usage: Usage,
    pub cache: crate::cache::CacheSummary,
    pub feedback: Option<feedback::Feedback>,
    pub execution: ExecutionState,
    /// Live prose admitted since this client observed the execution start.
    /// Unknown after a stream gap; an install, or a running presentation once a
    /// baseline is installed, that finds the execution running while it is
    /// unknown starts a new zero baseline, otherwise the existing baseline
    /// carries. Durable history never contributes.
    pub(crate) streamed_text_bytes: Option<usize>,
    /// An execution this client SAW interrupted on the live stream.
    ///
    /// This is the LIVE observation and it is not the durable outcome. Durable
    /// truth carries interruption through the structured error, never through
    /// `finish` — see `SessionView::durable_outcome` — so the two are
    /// separate facts arriving by separate routes, and this one is the fact
    /// that no rebuild can reconstruct: which execution THIS client watched
    /// stop.
    ///
    /// It is kept as a SEPARATE, EXPLICITLY HISTORICAL observation rather than
    /// as the current outcome, and it names the execution it belongs to. An
    /// earlier revision copied it into `execution` whenever a rebuild produced
    /// a terminal with no execution id — but a rebuild NEVER names one, so the
    /// guard matched every rebuild and would resurrect a stale interrupt over a
    /// later execution that had succeeded or failed elsewhere. Absence cannot
    /// establish sameness, and the protocol offers no way to correlate a live
    /// execution with a rebuilt terminal, so this does not try to.
    pub interrupted_observed: Option<crate::wire::ExecutionWireId>,
    /// Whether the session has reported quiescence, when it has reported it at
    /// all.
    ///
    /// Distinct from [`ExecutionState::Settled`]. A settled execution says one
    /// unit of work finished; idle says the session as a whole has nothing
    /// running. Collapsing them is what shows a settled run that never
    /// settled.
    ///
    /// ## Why `Option` rather than `bool`
    ///
    /// `session.idle` is one of the FIVE frozen members that carry no `durable`
    /// block, so quiescence is live truth that a durable re-list cannot carry.
    /// A rebuilt projection therefore has not been told either way — and this
    /// field was a `bool` initialised to `false`, so it said `busy`.
    ///
    /// That is not a cosmetic default. Reopening a session whose work finished
    /// hours ago rendered `session busy` and kept rendering it, because no idle
    /// frame was ever coming for a session where nothing was happening. The
    /// screen asserted activity the client could not observe, which is the
    /// exact failure the truthful-state law names.
    ///
    /// `None` is "not reported", and the renderer says so.
    pub idle: Option<bool>,
    /// Live payload-free inbox attention; unrelated to execution/quiescence.
    pub mail_attention: Option<grapt_api::u3a::MailAttentionInfo>,
    /// Live StateEngine continuity level, replaced by every frame. `None` is
    /// unknown (no frame on this connection), never healthy.
    pub continuity_attention: Option<grapt_api::u3a::ContinuityAttentionInfo>,
    /// The server stamp of the newest quiescence fact applied to this view,
    /// `None` while `idle` is `None`. See `idle` for why the fact is live-only.
    pub idle_created: Option<u64>,
    /// The durable seq of the frame that set `idle`; only the running edge has one.
    pub(crate) idle_seq: Option<u64>,
    /// The server stamp of the newest TERMINAL frame this view applied live.
    ///
    /// A durable rebuild carries no stamp, so this stays `None` on a view
    /// built from the re-list alone. It exists to order a carried quiescence
    /// fact against the terminal the rebuild speaks of: a fact stamped after
    /// the last live terminal is the settle's own word and survives the
    /// rebuild; one stamped before it is stale.
    pub terminal_created: Option<u64>,
    /// The durable seq of that live terminal.
    pub(crate) terminal_seq: Option<u64>,
    /// Slices evicted by the retention bound.
    pub dropped_slices: usize,
    /// The user rows this view has folded in, retained or evicted.
    ///
    /// Retention evicts the oldest slices and counts them in
    /// [`SessionView::dropped_slices`], so the retained count alone plateaus at the bound
    /// while the transcript keeps growing: a count of what is on screen is display
    /// policy, not lifecycle identity. A user row is pushed exactly once, by the durable
    /// User arm, and never again, so this is the eviction-independent measure of the
    /// turns the transcript names.
    pub folded_user_rows: usize,
    /// The newest durable terminal this view holds: the assistant record's
    /// identity and the outcome it settled.
    ///
    /// How an install tells a NEW terminal from a re-read of one it holds. A
    /// count of user rows cannot once a read covers only a window, and no user
    /// row need be in that window: one long prompt can fill a page with
    /// assistant records. Only a tail read's NEWEST record can move it, and an
    /// install whose newest record is not a terminal keeps the one held.
    /// Splicing older history in front never evaluates it, so backfill cannot
    /// make a terminal.
    pub held_terminal: Option<(crate::wire::MessageWireId, Outcome)>,
    /// The leading slices folded from history OLDER than the installed tail.
    pub older_len: usize,
    /// The installed tail page's rows, oldest first, each with the slices it
    /// folded to; empty when that is no longer known. A later tail read uses
    /// it to keep the scrollback strictly older than itself (`window.rs`).
    pub tail_rows: Vec<(crate::wire::MessageWireId, usize)>,
    /// Wire `(assistantMessageID, ordinal)` of each open block, by slice
    /// ordinal, taken from its start frame, with the digest of every byte it
    /// admitted.
    slice_wires: std::collections::BTreeMap<usize, proof::Wired>,
    live_progress: progress::Messages,
    next_ordinal: usize,
    footer_clock: footer::Clock,
    /// The kind of the slice an unfinished durable answer left open, until a
    /// live end closes it or a later row or execution seals it.
    adopted: Option<SliceKind>,
}

impl SessionView {
    pub fn new(session_id: &crate::wire::SessionWireId) -> Self {
        Self {
            queued: crate::wire::QueuedInputPreview::default(),
            interrupted_observed: None,
            session_id: session_id.clone(),
            slices: Vec::new(),
            slice_wires: std::collections::BTreeMap::new(),
            usage: Usage::default(),
            cache: crate::cache::CacheSummary::default(),
            feedback: None,
            execution: ExecutionState::Idle,
            streamed_text_bytes: None,
            idle: None,
            mail_attention: None,
            continuity_attention: None,
            idle_created: None,
            idle_seq: None,
            terminal_created: None,
            terminal_seq: None,
            dropped_slices: 0,
            folded_user_rows: 0,
            held_terminal: None,
            older_len: 0,
            tail_rows: Vec::new(),
            live_progress: Default::default(),
            next_ordinal: 0,
            footer_clock: footer::Clock::default(),
            adopted: None,
        }
    }

    /// Presentation estimate input, separate from provider-reported usage.
    pub fn streamed_text_bytes(&self) -> Option<usize> {
        matches!(self.execution, ExecutionState::Running { .. })
            .then_some(self.streamed_text_bytes)
            .flatten()
    }

    /// An install found the execution running with the estimate unknown: count
    /// from zero, attributing no earlier bytes to it.
    pub(crate) fn arm_live_output(&mut self) {
        if matches!(self.execution, ExecutionState::Running { .. }) {
            self.streamed_text_bytes.get_or_insert(0);
        }
    }

    /// The number of user rows in the transcript: one per turn. The
    /// durable re-list stamps no terminal, so the rows order the terminals
    /// it carries — a new turn's terminal arrives with its new row (the
    /// User arm), and a re-list of an unchanged history adds none.
    pub fn user_row_count(&self) -> usize {
        self.slices
            .iter()
            .filter(|slice| {
                matches!(
                    slice.kind,
                    SliceKind::User | SliceKind::Automated | SliceKind::SeatStartup
                )
            })
            .count()
    }

    /// Close the slice an unfinished durable answer left open. It is kept
    /// open for that answer's live deltas, but one left unfinished by an
    /// interrupt or a lost process must not take a later answer's text.
    fn seal_unfinished(&mut self) {
        if let Some(kind) = self.adopted.take() {
            if let Some(slice) = self.open_slice(kind) {
                slice.closed = true;
            }
        }
    }

    /// Fold a queued input the service promoted while this view is live. It
    /// is not a later row: an answer still streaming keeps its open slice.
    pub(crate) fn apply_promoted(
        &mut self,
        message: &crate::wire::DurableMessage,
        bounds: Bounds,
    ) -> Result<Applied, ProjectionError> {
        let adopted = self.adopted.take();
        let applied = self.apply_durable(message, bounds);
        self.adopted = adopted;
        applied
    }

    /// The open slice of one kind, if any.
    fn open_slice(&mut self, kind: SliceKind) -> Option<&mut Slice> {
        self.slices
            .iter_mut()
            .rev()
            .find(|slice| slice.kind == kind && !slice.closed)
    }
}

pub use offset::StreamMode;

/// What applying one event did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// The view changed.
    Changed,
    /// The event is outside this client's vocabulary or this session; the view
    /// is unchanged. Not an error — the union is larger than the journey.
    Ignored,
}

/// Why an event could not be applied.
///
/// These are transcript defects, not transport failures. Each one names a
/// shape the frozen contract forbids, and the reducer refuses rather than
/// guessing — a projection that repaired a malformed transcript would render
/// a history the engine never produced.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProjectionError {
    #[error("a {kind:?} delta carried no string delta payload")]
    DeltaPayloadMissing { kind: SliceKind },
    #[error("a {kind:?} delta arrived with no matching opened block")]
    DeltaWithoutOpen { kind: SliceKind },
    #[error("a {kind:?} delta skipped bytes its block never received")]
    OffsetGap { kind: SliceKind },
    #[error("a {kind:?} block's end does not match the bytes it admitted")]
    EndUnproven { kind: SliceKind },
    #[error("a {kind:?} end arrived with no matching opened block")]
    EndWithoutOpen { kind: SliceKind },
    #[error("a second {kind:?} block opened while one was still open")]
    OverlappingOpen { kind: SliceKind },
    #[error("a {kind:?} frame named a different block than the open one")]
    IdentityMismatch { kind: SliceKind },
    #[error("a {kind:?} frame named no well-formed assistantMessageID and ordinal")]
    IdentityMissing { kind: SliceKind },
    // These two carry TYPED identities and render them through an explicit
    // accessor rather than storing a `String` copy. An error is a rendering,
    // so a string here would have been defensible — but it would also have been
    // the one remaining place where an execution identity existed as loose
    // text, and "defensible" is how the flattening came back last time.
    #[error("execution `{}` reached a second durable terminal", .execution_id.spelled())]
    SecondTerminal {
        execution_id: crate::wire::ExecutionWireId,
    },
    #[error(
        "a terminal arrived for `{}` while `{}` was running",
        .observed.spelled(),
        .running.spelled()
    )]
    TerminalAttributionMismatch {
        running: crate::wire::ExecutionWireId,
        observed: crate::wire::ExecutionWireId,
    },
    #[error("an execution member carried no executionID")]
    ExecutionIdMissing,
    #[error("session.status carried a status payload the fold cannot map")]
    SessionStatusMalformed,
    #[error("session mail attention is malformed")]
    MailAttentionMalformed,
    #[error("session continuity attention is malformed")]
    ContinuityAttentionMalformed,
}

impl SessionView {
    /// Apply one decoded event.
    ///
    /// The caller is responsible for having admitted the event through
    /// [`crate::cursor::DurableCursor`] first; this function is the pure fold
    /// and knows nothing about sequences or gaps.
    pub fn apply(
        &mut self,
        envelope: &EventEnvelope,
        bounds: Bounds,
    ) -> Result<Applied, ProjectionError> {
        self.apply_in(envelope, bounds, StreamMode::Legacy)
    }

    /// Apply one decoded event that arrived on a stream of `mode`.
    pub fn apply_in(
        &mut self,
        envelope: &EventEnvelope,
        bounds: Bounds,
        mode: StreamMode,
    ) -> Result<Applied, ProjectionError> {
        let Some(member) = envelope.member else {
            return Ok(Applied::Ignored);
        };
        // Ownership, compared as TYPED identities. Two spellings of one
        // session can no longer disagree here, because there are no two
        // spellings: the envelope's id and this view's id were both produced by
        // the same parse.
        if let Some(session) = envelope.session_id.as_ref() {
            if session != &self.session_id {
                return Ok(Applied::Ignored);
            }
        }
        match member {
            EventMember::ServerConnected | EventMember::SessionCreated => Ok(Applied::Ignored),
            EventMember::TextStarted => self.open(SliceKind::Text, envelope, bounds, mode),
            EventMember::TextDelta => self.delta(SliceKind::Text, envelope, bounds, mode),
            EventMember::TextEnded => self.close(SliceKind::Text, envelope, bounds, mode),
            EventMember::ReasoningStarted => {
                self.open(SliceKind::Reasoning, envelope, bounds, mode)
            }
            EventMember::ReasoningDelta => self.delta(SliceKind::Reasoning, envelope, bounds, mode),
            EventMember::ReasoningEnded => self.close(SliceKind::Reasoning, envelope, bounds, mode),
            EventMember::UsageUpdated => {
                self.usage = Usage {
                    input: field_u64(envelope, "input"),
                    output: field_u64(envelope, "output"),
                };
                Ok(Applied::Changed)
            }
            EventMember::ExecutionStarted => {
                self.feedback = None;
                self.footer_clock.boundary();
                let execution_id = execution_id(envelope)?;
                let fresh = !matches!(&self.execution, ExecutionState::Running { execution_id: current }
                    if current == &execution_id);
                if fresh {
                    self.seal_unfinished();
                }
                if fresh || self.streamed_text_bytes.is_none() {
                    self.streamed_text_bytes = Some(0);
                }
                self.execution = ExecutionState::Running { execution_id };
                self.set_idle(false, envelope);
                Ok(Applied::Changed)
            }
            EventMember::ExecutionSucceeded => self.settle(envelope, Outcome::Succeeded, bounds),
            EventMember::ToolProgress => Ok(Applied::Changed),
            EventMember::ToolInputStarted
            | EventMember::ToolCalled
            | EventMember::ToolSuccess
            | EventMember::ToolFailed => Ok(Applied::Ignored),
            EventMember::ExecutionFailed => self.settle(envelope, Outcome::Failed, bounds),
            EventMember::ExecutionInterrupted => {
                self.settle(envelope, Outcome::Interrupted, bounds)
            }
            EventMember::SessionIdle => {
                // Quiescence, NOT a terminal. It does not settle an execution
                // and it does not invent an outcome for one still running.
                self.set_idle(true, envelope);
                Ok(Applied::Changed)
            }
            EventMember::ContinuityAttention => self.continuity(envelope),
            EventMember::MailAttention => {
                let status = envelope
                    .data
                    .get("status")
                    .cloned()
                    .ok_or(ProjectionError::MailAttentionMalformed)?;
                let status = serde_json::from_value(status)
                    .map_err(|_| ProjectionError::MailAttentionMalformed)?;
                // Cached counts remain available on the wire, but cannot be
                // presented as current attention while their source is stale.
                self.mail_attention = Some(
                    if envelope.data.get("staleSince").is_some()
                        && !matches!(
                            status,
                            grapt_api::u3a::MailAttentionInfo::Disabled
                                | grapt_api::u3a::MailAttentionInfo::RebindRequired
                                | grapt_api::u3a::MailAttentionInfo::Stopped
                        )
                    {
                        grapt_api::u3a::MailAttentionInfo::Retrying
                    } else {
                        status
                    },
                );
                Ok(Applied::Changed)
            }
            EventMember::SessionStatus => {
                // The SESSION-QUIESCENCE ruling: idle is the only quiescent state. A
                // retry frame means the provider will try again, so the session is not
                // quiescent either. Like session.idle this is a quiescence signal, not a
                // terminal: it never settles an execution or invents one an outcome.
                let status = envelope
                    .data
                    .get("status")
                    .and_then(|status| status.get("type"))
                    .and_then(serde_json::Value::as_str);
                let idle = match status {
                    Some("idle") => true,
                    Some("busy") | Some("retry") => false,
                    _ => return Err(ProjectionError::SessionStatusMalformed),
                };
                self.retry_feedback(envelope, bounds);
                self.set_idle(idle, envelope);
                Ok(Applied::Changed)
            }
        }
    }

    /// Replace the continuity level with the one this frame carries.
    fn continuity(&mut self, envelope: &EventEnvelope) -> Result<Applied, ProjectionError> {
        let status = envelope
            .data
            .get("status")
            .cloned()
            .ok_or(ProjectionError::ContinuityAttentionMalformed)?;
        self.continuity_attention = Some(
            serde_json::from_value(status)
                .map_err(|_| ProjectionError::ContinuityAttentionMalformed)?,
        );
        Ok(Applied::Changed)
    }

    /// The open slice a frame's identity names: deltas and ends go to the
    /// block their start named, never merely to the newest one of its kind.
    fn open_by_identity(&self, kind: SliceKind, identity: &SliceWire) -> Option<usize> {
        self.slices.iter().rposition(|slice| {
            slice.kind == kind
                && !slice.closed
                && self
                    .slice_wires
                    .get(&slice.ordinal)
                    .is_some_and(|wired| &wired.identity == identity)
        })
    }

    /// Claim the open slice a delta or end names. A slice of the kind open
    /// under no matching identity (rebuilt from history, or another block's)
    /// is a mismatch; no open slice of the kind at all is `absent`.
    fn claim_open(
        &self,
        kind: SliceKind,
        envelope: &EventEnvelope,
        absent: ProjectionError,
    ) -> Result<usize, ProjectionError> {
        let frame = wire_identity(envelope, kind)?;
        if let Some(index) = self.open_by_identity(kind, &frame) {
            return Ok(index);
        }
        if self
            .slices
            .iter()
            .any(|slice| slice.kind == kind && !slice.closed)
        {
            Err(ProjectionError::IdentityMismatch { kind })
        } else {
            Err(absent)
        }
    }

    fn open(
        &mut self,
        kind: SliceKind,
        envelope: &EventEnvelope,
        bounds: Bounds,
        mode: StreamMode,
    ) -> Result<Applied, ProjectionError> {
        // A live start of the adopted kind proves the read caught that block
        // already complete, between it and the next one.
        if self.adopted == Some(kind) {
            self.seal_unfinished();
        }
        let identity = wire_identity(envelope, kind)?;
        // Several blocks of one kind may be open at once: the producer holds
        // finished reasoning uncommitted (no end until it is durable) while
        // the next block opens. Only the same block opening twice overlaps.
        if self.open_by_identity(kind, &identity).is_some() {
            // A retained start the snapshot already opened is a duplicate.
            if mode == StreamMode::Positions {
                return Ok(Applied::Ignored);
            }
            return Err(ProjectionError::OverlappingOpen { kind });
        }
        self.push_open(kind, identity, String::new(), 0, bounds);
        Ok(Applied::Changed)
    }

    /// Push one open, identity-tagged slice holding `text`, then evict.
    fn push_open(
        &mut self,
        kind: SliceKind,
        identity: SliceWire,
        text: String,
        truncated_bytes: usize,
        bounds: Bounds,
    ) {
        let ordinal = self.next_ordinal;
        self.next_ordinal = self.next_ordinal.saturating_add(1);
        self.slices.push(Slice {
            compaction: None,
            read_detail_start: None,
            tool_summary: None,
            marks: Vec::new(),
            kind,
            tool_id: None,
            tool_phase: None,
            ordinal,
            text,
            closed: false,
            footer: None,
            truncated_bytes,
        });
        self.slice_wires
            .insert(ordinal, proof::Wired::new(identity));
        // Evict from the front, and COUNT the eviction. An unbounded
        // transcript is named in the client matrix as something to delete
        // rather than preserve; a silent one is worse than either.
        while self.slices.len() > bounds.max_retained_slices {
            let evicted = self.slices.remove(0);
            self.slice_wires.remove(&evicted.ordinal);
            self.dropped_slices = self.dropped_slices.saturating_add(1);
            self.evicted_front(1);
        }
    }

    fn delta(
        &mut self,
        kind: SliceKind,
        envelope: &EventEnvelope,
        bounds: Bounds,
        mode: StreamMode,
    ) -> Result<Applied, ProjectionError> {
        let text = envelope
            .data
            .get("delta")
            .and_then(serde_json::Value::as_str)
            .ok_or(ProjectionError::DeltaPayloadMissing { kind })?
            .to_owned();
        // The slice the delta names, by identity; then its offset, if any.
        let index =
            match self.claim_open(kind, envelope, ProjectionError::DeltaWithoutOpen { kind }) {
                Ok(index) => index,
                Err(_)
                    if mode == StreamMode::Positions && self.superseded_delta(envelope, kind) =>
                {
                    return Ok(Applied::Ignored);
                }
                Err(error) => return Err(error),
            };
        let Some(text) = self.admit_delta(kind, index, envelope, text, mode)? else {
            return Ok(Applied::Ignored);
        };
        let slice = &mut self.slices[index];
        let room = bounds.max_slice_bytes.saturating_sub(slice.text.len());
        if text.len() <= room {
            slice.text.push_str(&text);
        } else {
            // Truncate on a character boundary so the retained text stays
            // valid UTF-8, and record exactly how much was refused.
            let mut cut = room;
            while cut > 0 && !text.is_char_boundary(cut) {
                cut -= 1;
            }
            slice.text.push_str(&text[..cut]);
            slice.truncated_bytes = slice.truncated_bytes.saturating_add(text.len() - cut);
        }
        if kind == SliceKind::Text && matches!(self.execution, ExecutionState::Running { .. }) {
            self.streamed_text_bytes = self
                .streamed_text_bytes
                .map(|bytes| bytes.saturating_add(text.len()));
        }
        Ok(Applied::Changed)
    }

    fn close(
        &mut self,
        kind: SliceKind,
        envelope: &EventEnvelope,
        bounds: Bounds,
        mode: StreamMode,
    ) -> Result<Applied, ProjectionError> {
        if mode == StreamMode::Positions {
            return self.close_positioned(kind, envelope, bounds);
        }
        let index = self.claim_open(kind, envelope, ProjectionError::EndWithoutOpen { kind })?;
        let slice = &mut self.slices[index];
        slice.closed = true;
        let ordinal = slice.ordinal;
        self.slice_wires.remove(&ordinal);
        if self.adopted == Some(kind) {
            self.adopted = None;
        }
        Ok(Applied::Changed)
    }

    fn settle(
        &mut self,
        envelope: &EventEnvelope,
        outcome: Outcome,
        bounds: Bounds,
    ) -> Result<Applied, ProjectionError> {
        let observed = execution_id(envelope)?;
        match &self.execution {
            // `Some(&observed)`, not a bare comparison: a view settled by the
            // DURABLE re-list carries no execution id, and it must NOT swallow
            // the first live terminal that arrives afterwards as a duplicate.
            // The previous revision got this right only by accident, because
            // the invented `"durable"` string happened never to equal a real
            // execution id.
            ExecutionState::Settled { execution_id, .. }
                if execution_id.as_ref() == Some(&observed) =>
            {
                Err(ProjectionError::SecondTerminal {
                    execution_id: observed,
                })
            }
            ExecutionState::Running { execution_id } if execution_id != &observed => {
                Err(ProjectionError::TerminalAttributionMismatch {
                    running: execution_id.clone(),
                    observed,
                })
            }
            _ => {
                if outcome == Outcome::Interrupted {
                    self.interrupted_observed = Some(observed.clone());
                }
                self.terminal_feedback(envelope, &outcome, bounds);
                self.terminal_created = envelope.created;
                self.terminal_seq = FactStamp::of(envelope).seq;
                self.execution = ExecutionState::Settled {
                    execution_id: Some(observed),
                    outcome,
                };
                Ok(Applied::Changed)
            }
        }
    }
}

impl SessionView {
    /// Fold one DURABLE message from the real re-list.
    ///
    /// This is the other half of the convergence claim. The live stream
    /// produces text and reasoning slices; the durable re-list produces the
    /// same content as an assistant message's ordered items. Folding both into
    /// the same `Slice` model is what makes "the projection equals durable
    /// truth" a comparison rather than an aspiration.
    pub fn apply_durable(
        &mut self,
        message: &crate::wire::DurableMessage,
        bounds: Bounds,
    ) -> Result<Applied, ProjectionError> {
        use crate::wire::{DurableMessage, MessageItem};
        // Any later row ends whatever answer was left unfinished before it.
        self.seal_unfinished();
        match message {
            DurableMessage::User { text }
            | DurableMessage::Automated { text }
            | DurableMessage::SeatStartup { text } => {
                self.feedback = None;
                self.footer_clock.boundary();
                // A NEW TURN BEGINS, so the previous turn's terminal stops
                // describing the present.
                //
                // This covers a window the assistant arm cannot: a re-list
                // taken just after a prompt was submitted carries the USER row
                // while that turn's assistant record does not exist yet, so
                // there is no later assistant record to clear anything. That
                // window has its own witness.
                self.execution = ExecutionState::Idle;
                // The turn names itself: count it before retention can
                // evict the row that proves it (SQ-H3 retention).
                self.folded_user_rows = self.folded_user_rows.saturating_add(1);
                let kind = match message {
                    DurableMessage::Automated { .. } => SliceKind::Automated,
                    DurableMessage::SeatStartup { .. } => SliceKind::SeatStartup,
                    _ => SliceKind::User,
                };
                self.push_slice(kind, text, bounds);
                Ok(Applied::Changed)
            }
            DurableMessage::ExecutionFailure { text } => {
                self.feedback = Some(feedback::Feedback::Failed {
                    message: feedback::bounded(text, bounds),
                });
                self.push_slice(SliceKind::System, text, bounds);
                // The turn's terminal: it failed before any assistant message,
                // so nothing else will settle it.
                self.execution = ExecutionState::Settled {
                    execution_id: None,
                    outcome: Outcome::Failed,
                };
                Ok(Applied::Changed)
            }
            DurableMessage::Compaction { info, text } => {
                self.push_slice(SliceKind::System, text, bounds);
                if let Some(slice) = self.slices.last_mut() {
                    slice.closed = info.phase != crate::wire::CompactionPhase::Running;
                    slice.compaction = Some(Box::new(info.clone()));
                }
                Ok(Applied::Changed)
            }
            DurableMessage::System { text } => {
                self.push_slice(SliceKind::System, text, bounds);
                Ok(Applied::Changed)
            }
            DurableMessage::Unsupported { kind } => {
                self.push_slice(SliceKind::Unsupported, kind, bounds);
                Ok(Applied::Changed)
            }
            DurableMessage::Assistant {
                items,
                finish,
                usage,
                error,
                footer,
                ..
            } => {
                if let Some(cache) = message.cache_usage() {
                    self.cache.observe(cache);
                }
                for item in items {
                    // ONE ordered sequence. A tool is pushed where it sits in
                    // the content array, not gathered to the end, or an answer
                    // would appear above the work it describes.
                    let (kind, text) = match item {
                        MessageItem::Text(text) => (SliceKind::Text, text),
                        MessageItem::Reasoning(text) => (SliceKind::Reasoning, text),
                        MessageItem::Tool(tool) => {
                            self.push_tool(tool, bounds);
                            continue;
                        }
                    };
                    self.push_slice(kind, text, bounds);
                }
                // An answer still streaming (no finish yet, or the `unknown`
                // every in-flight message carries) keeps its last text open,
                // so the live deltas after a mid-answer re-read continue it;
                // closed, each one was refused and sent the client back to
                // re-reading, and the rest of the answer never showed.
                let in_flight = error.is_none()
                    && matches!(finish, None | Some(crate::wire::DurableFinish::Unknown));
                if in_flight
                    && items.last().is_some_and(|item| {
                        matches!(item, MessageItem::Text(_) | MessageItem::Reasoning(_))
                    })
                {
                    if let Some(last) = self.slices.last_mut() {
                        last.closed = false;
                        self.adopted = Some(last.kind);
                    }
                }
                // The DURABLE terminal. Without this a reconciled transcript
                // would lose its outcome on every reconnect, because the live
                // `session.execution.*` events are not in the re-list.
                // Applied BEFORE the terminal, so the last assistant message's
                // accounting is what the screen shows — the same order the live
                // stream produces it in.
                if let Some((input, output)) = usage {
                    self.usage = Usage {
                        input: Some(*input),
                        output: Some(*output),
                    };
                }
                self.push_footer(footer.as_deref(), finish.as_ref(), error.as_ref(), bounds);
                // A terminal here settles the CURRENT turn. The previous
                // turn's outcome was already cleared by that turn's user row —
                // see the `User` arm, which is where a new turn begins.
                //
                // Clearing again HERE was tried and removed: it was
                // unmeasurable, because an assistant record always follows the
                // user record that has already cleared, so no real transcript
                // reaches it. An unmeasured guard reads as a second proven
                // protection and is not one.
                //
                // THE DEFECT THIS PAIR EXISTS FOR, observed against the real
                // product: an edit permission outstanding and `tool edit
                // running` on screen while the footer read `durable succeeded`
                // — the SHELL turn's terminal, describing a turn that had
                // ended.
                //
                // Records are applied in order, so the LAST one describes where
                // the session now stands. An assistant record carrying no
                // terminal is a turn that has not finished, and leaving the
                // previous turn's outcome in place made the screen report it as
                // the current one: observed against the real product with an
                // edit permission outstanding and `tool edit running` on
                // screen, while the footer read `durable succeeded` — the
                // SHELL turn's terminal, describing a turn that had ended.
                //
                // Clearing to Idle is not inventing correlation and does not
                // resurrect anything: it says durable truth offers no terminal
                // for the turn in progress, which is exactly true. The live
                // execution is then carried across the rebuild by `reconcile`,
                // and a real durable terminal still wins whenever one arrives.
                if let Some(outcome) = Self::durable_outcome(finish.as_ref(), error.as_ref()) {
                    self.feedback = feedback::Feedback::durable(&outcome, error.as_ref(), bounds);
                    self.execution = ExecutionState::Settled {
                        // No execution id, because the re-list names none. See
                        // `ExecutionState::Settled` for why this is `None`
                        // rather than the invented `"durable"` it replaces.
                        execution_id: None,
                        outcome,
                    };
                }
                Ok(Applied::Changed)
            }
        }
    }

    /// Append one already-complete slice.
    ///
    /// Durable content arrives whole rather than as deltas, so it is pushed
    /// CLOSED — an open slice means "still streaming", and a re-listed message
    /// is finished by definition.
    fn push_slice(&mut self, kind: SliceKind, text: &str, bounds: Bounds) {
        let ordinal = self.next_ordinal;
        self.next_ordinal = self.next_ordinal.saturating_add(1);
        let mut retained = String::new();
        let mut truncated_bytes = 0usize;
        if text.len() <= bounds.max_slice_bytes {
            retained.push_str(text);
        } else {
            let mut cut = bounds.max_slice_bytes;
            while cut > 0 && !text.is_char_boundary(cut) {
                cut -= 1;
            }
            retained.push_str(&text[..cut]);
            truncated_bytes = text.len() - cut;
        }
        self.slices.push(Slice {
            compaction: None,
            read_detail_start: None,
            tool_summary: None,
            marks: Vec::new(),
            kind,
            tool_id: None,
            tool_phase: None,
            ordinal,
            text: retained,
            closed: true,
            footer: None,
            truncated_bytes,
        });
        while self.slices.len() > bounds.max_retained_slices {
            let evicted = self.slices.remove(0);
            self.slice_wires.remove(&evicted.ordinal);
            self.dropped_slices = self.dropped_slices.saturating_add(1);
            self.evicted_front(1);
        }
    }
}

/// `data.executionID`, refused when absent.
///
/// The frozen record declares it on all four `session.execution.*` members. A
/// projection that tolerated its absence could not tell one execution settling
/// twice from two executions settling once each.
fn execution_id(envelope: &EventEnvelope) -> Result<crate::wire::ExecutionWireId, ProjectionError> {
    envelope
        .execution_id
        .clone()
        .ok_or(ProjectionError::ExecutionIdMissing)
}

fn field_u64(envelope: &EventEnvelope, field: &str) -> Option<u64> {
    // Absent is ABSENT, never zero. The usage law is explicit that coercing a
    // missing field to zero is a defect, because it turns "the provider did
    // not say" into "the provider said none".
    envelope.data.get(field).and_then(serde_json::Value::as_u64)
}

#[cfg(test)]
mod tests;

/// The block a live slice frame names: its assistant message and ordinal.
type SliceWire = (crate::wire::MessageWireId, u64);

/// A slice frame's `(assistantMessageID, ordinal)`. Every start, delta and
/// end carries both; an absent or malformed one is refused, never skipped.
fn wire_identity(envelope: &EventEnvelope, kind: SliceKind) -> Result<SliceWire, ProjectionError> {
    let message = envelope
        .data
        .get("assistantMessageID")
        .and_then(serde_json::Value::as_str)
        .and_then(|raw| crate::wire::MessageWireId::parse(raw.to_owned()));
    let ordinal = envelope
        .data
        .get("ordinal")
        .and_then(serde_json::Value::as_u64);
    message
        .zip(ordinal)
        .ok_or(ProjectionError::IdentityMissing { kind })
}
