//! The production [`AttemptOwner`]: the seam where a Step's normalized
//! fragments become the Session's durable assistant message.
//!
//! Law: `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md`
//! §"Attempt lifecycle, tools, and durable order" steps 4 and 7 ("append
//! durable facts/projection updates in owner order, then offer live
//! notification after commit"; "append exactly one Step terminal fact, commit
//! it with its projection"); rule 4 ("A Step has one and only one durable
//! terminal fact"); §"Normalized fragment vocabulary" ("Text/reasoning/input
//! deltas may be live-only to avoid durable projection churn, but close/
//! terminal facts must produce a replayable canonical projection… the Rust
//! runtime may not quietly accumulate an unbounded string just to imitate the
//! old publisher map"); and the ordered-content row of the supersession matrix.
//!
//! # What existed before this, and why it was not enough
//!
//! [`DurableFact`] carries its own warning: it is
//! "PROVISIONAL SHAPE — not a durable schema". Until this module there was no
//! production implementor of [`AttemptOwner`] at all — only
//! [`RecordingOwner`](crate::owner::RecordingOwner) for witnesses and a
//! store-backed double inside the session-application tests that appended every
//! fact under one borrowed kind string. So a Step's text, reasoning and usage
//! reached the owner and stopped there. This module is the other half: it maps
//! the provisional vocabulary onto the durable one.
//!
//! # One message per Step, committed before provider I/O
//!
//! The first admitted attempt commits the assistant opening before the provider
//! reader starts. Later attempts reuse that Step's identity, including retries
//! before visible output. An empty or failed response therefore still has a
//! replayable opening; live consumers never need a speculative message identity.
//!
//! Because it is held in the owner and the owner runs in ONE serialized loop,
//! the stamping is serialized by construction rather than by a lock: there is
//! no second writer that could interleave an ordinal.
//!
//! # Deltas accumulate to a BOUND, then close into one item
//!
//! A text or reasoning block emits many deltas and closes once. One durable
//! item per delta would put stream chatter in canonical history; one item per
//! BLOCK is the replayable projection the contract asks for. The accumulator is
//! bounded by [`MAX_BLOCK_CAPTURE_BYTES`] and a block that exceeds it is
//! captured truncated at the boundary — a typed
//! [`CaptureDisposition::TruncatedAtBoundary`], never a silent trim and never
//! an unbounded `String`.
//!
//! # What this owner deliberately does NOT write
//!
//! * **Attempt terminals.** `AttemptEnded`/`AttemptFailed` are attempt-level
//!   facts and their durable home is the operational attempt receipt, which
//!   `grapt-store` already owns through its own lifecycle. Writing them here
//!   would be a second route to a record another owner settles.
//! * **The attempt-start fact.** Same reason.
//! * **Live notification.** [`AttemptOwner::notify`] hands committed facts to
//!   an injected sink and nothing else; a bounded lossy fanout is the event
//!   feed's business, downstream of this lane.

use std::sync::{Arc, Mutex};

use grapt_artifact::{ArtifactKind, ArtifactStore, CaptureBound, Sensitivity};
use grapt_event::session_message::{
    AssistantMessageOpenedFact, ContentAppendedFact, ContentBlockKind, ContentBlockOpenedFact,
    UsageObservedFact,
};
use grapt_event::session_step_ended::StepEndedFact;
use grapt_event::session_step_failed::StepFailedFact;
use grapt_event::session_tool_terminal::ToolTerminalFact;
use grapt_event::AggregateId;
use grapt_inbox::Inbox;
use grapt_provider::{AttemptFragment, NormalizedFinish, TokenUsage};
use grapt_session::CommittedPosition;
use grapt_store::{EventStore, FencedWriter, WriteError};
use grapt_types::{
    AttemptUsage, CaptureDisposition, ClockAuthority, ContentKind, ContentOrdinal, EventId,
    IdAuthority, MessageId, NormalizedContentItem, PhysicalAttemptId, StepNumber, ToolCallId,
    UsageObservationPhase,
};

use crate::owner::{AttemptOwner, DurableFact, Observation, OwnerError, ToolOutcome};

/// The hard pre-allocation bound on ONE text or reasoning block's capture.
///
/// Named, not guessed at a call site. One MiB: a block is several deltas by
/// construction, and the bound must stay above the product's 256 KiB public
/// content bound so ordinary long output is captured whole. It is its own
/// literal, not a multiple of the coalesced-delta ceiling: that ceiling is
/// sized to one live event, and shrinking it must not shrink what a block may
/// capture.
pub const MAX_BLOCK_CAPTURE_BYTES: u64 = 1024 * 1024;

/// The working-set buffer one capture allocates. Independent of the ceiling
/// above: a large block still costs one small buffer.
pub const BLOCK_CAPTURE_BUFFER_BYTES: usize = 64 * 1024;

/// The hard bound on one tool call's captured input.
pub const MAX_TOOL_INPUT_CAPTURE_BYTES: u64 = 256 * 1024;

/// The most finished reasoning, replay bytes included, an attempt may hold
/// uncommitted before it has visible output. Past it the held reasoning is
/// committed, and the attempt counts as visible: its retry would no longer be
/// built from the same history.
pub const MAX_HELD_REASONING_BYTES: u64 = MAX_BLOCK_CAPTURE_BYTES;

/// The most finished reasoning blocks an attempt may hold, under the same
/// rule. The live projection holds at most as many awaiting their capture.
pub const MAX_HELD_REASONING_BLOCKS: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum DurableOwnerError {
    #[error("the identity authority produced an unusable {what} token")]
    Identity { what: &'static str },
    #[error("could not elect a writer for session {session}: {source}")]
    Election {
        session: String,
        #[source]
        source: WriteError,
    },
}

/// Where a committed fact is offered after its transaction commits.
///
/// Injected rather than assumed: this owner must not decide what live delivery
/// means, and a sink that did nothing would be indistinguishable from a
/// forgotten call. The trait makes the absence a CHOICE a caller states.
pub trait CommittedFactSink: std::fmt::Debug {
    fn offer(&mut self, fact: &DurableFact);

    /// Optional typed content notices, synchronous and strictly after commit.
    /// Coordinates come from the append result, never from a subsequent lookup
    /// by a normalized ordinal (which a legal stream may reuse after close).
    fn content_opened(&mut self, _position: &CommittedPosition, _fact: &ContentBlockOpenedFact) {}
    fn content_captured(&mut self, _position: &CommittedPosition, _fact: &ContentAppendedFact) {}
    fn step_began(&mut self, _step: StepNumber) {}
    fn step_settled(&mut self, _position: &CommittedPosition, _fact: &DurableFact) {}
    /// The failed attempt's open blocks were discarded uncommitted; the Step
    /// retries. A live projection of them is now stale.
    fn open_blocks_discarded(&mut self) {}
}

/// A sink that drops every offer, for a caller with no live subscriber.
#[derive(Debug, Default)]
pub struct NoLiveSubscribers;

impl CommittedFactSink for NoLiveSubscribers {
    fn offer(&mut self, _fact: &DurableFact) {}
}

mod checkpoint;
mod content;
mod hosted;
mod nested;
mod persistence;
mod resume;
mod tool_input;

// Re-exported rather than merely `pub mod`: the resumption vocabulary is part
// of this module's public surface and moving it to its own file is a layout
// decision, not a contract change. A caller's path is unchanged.
pub use resume::{
    ContinuationAuthority, PriorAttempt, ProcessLossReconciliation, ResumeRefused, ResumedMessage,
};

/// An open text or reasoning block, accumulating toward one durable item.
#[derive(Debug)]
struct OpenBlock {
    kind: ContentKind,
    text: String,
    /// Bytes the block produced BEYOND the capture bound. Non-zero means the
    /// item settles as truncated at the boundary.
    dropped: u64,
    /// The text block's last durable checkpoint (CLAIM-2).
    checkpoint: checkpoint::Checkpointed,
}

/// A finished reasoning block held until its attempt shows visible output or
/// settles (SC-REL-02c): a retried attempt's finished reasoning is discarded.
#[derive(Debug)]
struct HeldReasoning {
    block: OpenBlock,
    replay: Option<Vec<u8>>,
}

impl HeldReasoning {
    fn bytes(&self) -> u64 {
        (self.block.text.len() + self.replay.as_ref().map_or(0, Vec::len)) as u64
    }
}

/// The production owner.
///
/// Generic over the artifact store because [`ArtifactStore::capture`] is
/// generic and therefore not object-safe. The event store is shared behind a
/// `Mutex` because the inbox and the drain hold the same store; the guard is
/// taken and released inside one synchronous method, so it physically cannot
/// reach an `.await`.
#[derive(Debug)]
pub struct DurableAttemptOwner<'a, A: ArtifactStore, S: CommittedFactSink> {
    timing: Option<Arc<dyn grapt_types::timing::TimingObserver>>,
    selection: Option<grapt_event::session_message::AssistantSelection>,
    store: DurableStore,
    artifacts: A,
    clock: ClockCustody<'a>,
    ids: IdCustody<'a>,
    sink: S,
    session: grapt_types::SessionId,
    step: StepNumber,
    /// The Step's assistant message, once its first attempt committed the opening.
    message: Option<MessageId>,
    /// The next ordinal this message may claim. Serialized by the owner loop.
    next_ordinal: ContentOrdinal,
    /// Text and reasoning blocks currently open, keyed by their block ordinal.
    blocks: Vec<(u32, OpenBlock)>,
    /// The most bytes any ONE block has ever held in working memory.
    ///
    /// A high-water mark rather than a current reading, because the reading a
    /// witness can take after the block closes is always zero. This is the
    /// coordinate that proves the accumulator is bounded INDEPENDENTLY of the
    /// item it produced: the captured artifact's length is capped by the
    /// capture boundary whatever the producer did, so length alone cannot
    /// separate a bounded accumulator from an unbounded one that handed the
    /// store too many bytes. This can.
    peak_block_bytes: u64,
    /// The Step's terminal, once written. A second is refused here as well as
    /// in the read fold: the owner is where it can still be prevented.
    settled: bool,
    /// The attempt currently streaming, for facts that name one.
    attempt: Option<PhysicalAttemptId>,
    /// Whether the current attempt produced visible output, by the
    /// validator's own predicate. Past it, a failure stops the Step.
    attempt_visible: bool,
    /// Whether the current attempt ended in a failure the Step may retry.
    retryable_failure: bool,
    /// Finished reasoning of the current attempt, in close order, not yet
    /// committed because nothing visible followed it.
    held: Vec<HeldReasoning>,
    /// The bytes `held` carries, replay included.
    held_bytes: u64,
    /// On a RESUMED Step, the one attempt the continuation authority named.
    /// `None` on a fresh Step, where any first attempt is legitimate because
    /// nothing was continued.
    continuation: Option<PhysicalAttemptId>,
}

#[derive(Debug)]
enum DurableStore {
    Event(Arc<Mutex<EventStore>>),
    Inbox(Arc<Mutex<Inbox>>),
}

enum ClockCustody<'a> {
    Borrowed(&'a (dyn ClockAuthority + Send + Sync)),
    Owned(Arc<dyn ClockAuthority + Send + Sync>),
}

impl std::fmt::Debug for ClockCustody<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("<clock-authority>")
    }
}

impl ClockCustody<'_> {
    fn authority(&self) -> &dyn ClockAuthority {
        match self {
            Self::Borrowed(authority) => *authority,
            Self::Owned(authority) => authority.as_ref(),
        }
    }
}

enum IdCustody<'a> {
    Borrowed(&'a (dyn IdAuthority + Send + Sync)),
    Owned(Arc<dyn IdAuthority + Send + Sync>),
}

impl std::fmt::Debug for IdCustody<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("<id-authority>")
    }
}

impl IdCustody<'_> {
    fn authority(&self) -> &dyn IdAuthority {
        match self {
            Self::Borrowed(authority) => *authority,
            Self::Owned(authority) => authority.as_ref(),
        }
    }
}

enum DurableStoreGuard<'a> {
    Event(std::sync::MutexGuard<'a, EventStore>),
    Inbox(std::sync::MutexGuard<'a, Inbox>),
}

impl std::ops::Deref for DurableStoreGuard<'_> {
    type Target = EventStore;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Event(guard) => guard,
            Self::Inbox(guard) => guard.store(),
        }
    }
}

impl std::ops::DerefMut for DurableStoreGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Self::Event(guard) => guard,
            Self::Inbox(guard) => guard.store_mut(),
        }
    }
}

impl DurableStore {
    fn lock(&self) -> DurableStoreGuard<'_> {
        match self {
            Self::Event(store) => DurableStoreGuard::Event(lock_event(store)),
            Self::Inbox(inbox) => DurableStoreGuard::Inbox(match inbox.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            }),
        }
    }
}

impl<'a, A: ArtifactStore, S: CommittedFactSink> DurableAttemptOwner<'a, A, S> {
    /// Constructs the owner and ELECTS its writer.
    ///
    /// Election happens here rather than lazily on first commit because it is
    /// an ownership act: a Step that cannot own its aggregate must fail before
    /// a provider stream opens, not after it has produced output nobody can
    /// store. There is deliberately no zero-argument constructor — a durable
    /// owner that forgot its authorities does not compile.
    #[allow(clippy::too_many_arguments)]
    pub fn elect(
        store: Arc<Mutex<EventStore>>,
        artifacts: A,
        clock: &'a (dyn ClockAuthority + Send + Sync),
        ids: &'a (dyn IdAuthority + Send + Sync),
        sink: S,
        session: grapt_types::SessionId,
        step: StepNumber,
        resume: Option<ResumedMessage>,
    ) -> Result<Self, DurableOwnerError> {
        Self::elect_over(
            DurableStore::Event(store),
            artifacts,
            ClockCustody::Borrowed(clock),
            IdCustody::Borrowed(ids),
            sink,
            session,
            step,
            resume,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn elect_over(
        store: DurableStore,
        artifacts: A,
        clock: ClockCustody<'a>,
        ids: IdCustody<'a>,
        sink: S,
        session: grapt_types::SessionId,
        step: StepNumber,
        resume: Option<ResumedMessage>,
    ) -> Result<Self, DurableOwnerError> {
        let aggregate = AggregateId::Session(session.clone());
        {
            let mut guard = store.lock();
            guard
                .elect_writer(&aggregate)
                .map_err(|source| DurableOwnerError::Election {
                    session: session.as_str().to_owned(),
                    source,
                })?;
        }
        Ok(Self {
            store,
            selection: None,
            timing: None,
            artifacts,
            clock,
            ids,
            sink,
            session,
            step,
            // A resumed Step adopts the message it already owns and continues
            // its ordinals; a fresh one mints on first content and starts at
            // the first ordinal. There is no third combination.
            message: resume.as_ref().map(|held| held.message().clone()),
            next_ordinal: match &resume {
                Some(held) => held.next_ordinal(),
                None => ContentOrdinal::FIRST,
            },
            continuation: resume
                .as_ref()
                .map(|held| held.continuation_attempt().clone()),
            blocks: Vec::new(),
            peak_block_bytes: 0,
            settled: false,
            attempt: None,
            attempt_visible: false,
            retryable_failure: false,
            held: Vec::new(),
            held_bytes: 0,
        })
    }

    /// The most bytes one open block has held. See [`Self::peak_block_bytes`]'s
    /// field documentation: this is the working-memory reading, not the
    /// captured length.
    pub fn peak_block_bytes(&self) -> u64 {
        self.peak_block_bytes
    }

    /// The Step's assistant message, if its opening has committed.
    pub fn message(&self) -> Option<&MessageId> {
        self.message.as_ref()
    }

    pub fn with_timing(mut self, timing: Arc<dyn grapt_types::timing::TimingObserver>) -> Self {
        self.timing = Some(timing);
        self
    }

    pub fn with_selection(
        mut self,
        selection: grapt_event::session_message::AssistantSelection,
    ) -> Result<Self, OwnerError> {
        if self.message.is_some() || self.attempt.is_some() {
            return Err(OwnerError {
                reason: "cannot replace an opened or resumed Step selection".into(),
            });
        }
        self.selection = Some(selection);
        Ok(self)
    }

    /// Whether `failure` ends an attempt the Step answers with another one: a
    /// provider failure that may retry, or a no-progress stall, before any
    /// visible output. Every retryable failure qualifies, including the last
    /// one the bound refuses, so no unfinished block outlives its attempt.
    /// The attempt's own terminal decides: an interrupt that settles an
    /// attempt after its retryable failure arrived is still an interrupt, and
    /// keeps its partial.
    fn retries(&self, failure: &grapt_provider::AttemptFailure) -> bool {
        use grapt_types::{AttemptTerminalReason, ProviderFailureClass};
        !self.attempt_visible
            && match failure.reason() {
                AttemptTerminalReason::ProviderFailure {
                    class: ProviderFailureClass::NoProgressTimeout,
                } => true,
                AttemptTerminalReason::ProviderFailure { .. } => self.retryable_failure,
                _ => false,
            }
    }

    /// Whether this Step has already written its one terminal.
    pub fn is_settled(&self) -> bool {
        self.settled
    }

    fn mint<T, F>(&self, what: &'static str, build: F) -> Result<T, OwnerError>
    where
        F: Fn(&str) -> Option<T>,
    {
        build(self.ids.authority().mint().as_str()).ok_or_else(|| OwnerError {
            reason: format!("the identity authority produced an unusable {what} token"),
        })
    }

    fn event_id(&self) -> Result<EventId, OwnerError> {
        self.mint("event", |token| format!("evt_{token}").parse().ok())
    }

    fn now_ms(&self) -> i64 {
        self.clock.authority().now().as_millis()
    }

    fn writer(&self, store: &EventStore) -> Result<FencedWriter, OwnerError> {
        store
            .fencing_token(&AggregateId::Session(self.session.clone()))
            .map_err(|source| OwnerError {
                reason: source.to_string(),
            })
    }

    fn open_message(&mut self, attempt: &PhysicalAttemptId) -> Result<(), OwnerError> {
        if self.message.is_some() {
            return Ok(());
        }
        let message: MessageId =
            self.mint("message", |token| format!("msg_{token}").parse().ok())?;
        let id = self.event_id()?;
        let opening = AssistantMessageOpenedFact::new(attempt.clone(), self.step, message.clone());
        let opening = match &self.selection {
            Some(selection) => opening.with_selection(selection.clone()),
            None => opening,
        };
        {
            let mut guard = self.store.lock();
            let writer = self.writer(&guard)?;
            let tx = guard.governed_transaction().map_err(|source| OwnerError {
                reason: source.to_string(),
            })?;
            grapt_store::session_message::record_opened(
                &tx,
                &self.session,
                &id,
                writer,
                self.now_ms(),
                &opening,
                &mut [],
            )
            .map_err(write)?;
            tx.commit()
                .map_err(|source| OwnerError {
                    reason: source.to_string(),
                })?
                .discard();
        }
        // Publication and retry reuse can see this identity only after commit.
        self.message = Some(message);
        Ok(())
    }

    fn append_tool_terminal(
        &mut self,
        call: &ToolCallId,
        outcome: &ToolOutcome,
    ) -> Result<(), OwnerError> {
        let attempt = match &self.attempt {
            Some(attempt) => attempt.clone(),
            None => {
                return Err(OwnerError {
                    reason: "a tool terminal arrived before any attempt opened".to_owned(),
                })
            }
        };
        let fact = ToolTerminalFact::new(attempt, call.clone(), outcome.clone());
        let id = self.event_id()?;
        let now = self.now_ms();
        let mut guard = self.store.lock();
        let writer = self.writer(&guard)?;
        let tx = guard.governed_transaction().map_err(|source| OwnerError {
            reason: source.to_string(),
        })?;
        grapt_store::tool_terminal::record(&tx, &self.session, &id, writer, now, &fact, &mut [])
            .map_err(write)?;
        tx.commit()
            .map_err(|source| OwnerError {
                reason: source.to_string(),
            })?
            .discard();
        Ok(())
    }

    /// Writes the Step's ONE terminal.
    ///
    /// The guard is here and not only in the read fold because this is the last
    /// place a second terminal can still be prevented. A fold that refuses one
    /// proves the log is readable; an owner that refuses one keeps the log
    /// clean.
    fn settle(&mut self, terminal: Terminal) -> Result<(), OwnerError> {
        if self.settled {
            return Err(OwnerError {
                reason: format!(
                    "step {} already wrote its one durable terminal",
                    self.step.value()
                ),
            });
        }
        // Cancellation can arrive between deltas and the provider's close.
        // Capture those already observed bytes before committing the terminal,
        // so reconnecting cannot erase output the user saw while streaming.
        let capture_failure = self.prepare_step_terminal().err();
        let terminal = if capture_failure.is_some() {
            Terminal::Failed(
                StepFailedFact::new(
                    self.step,
                    grapt_types::StepTerminalReason::DurabilityFailure,
                )
                .map_err(|error| OwnerError {
                    reason: error.to_string(),
                })?,
            )
        } else {
            terminal
        };
        let id = self.event_id()?;
        let now = self.now_ms();
        {
            let mut guard = self.store.lock();
            let writer = self.writer(&guard)?;
            let tx = guard.governed_transaction().map_err(|source| OwnerError {
                reason: source.to_string(),
            })?;
            match &terminal {
                Terminal::Ended(fact) => {
                    grapt_store::step_ended::record(
                        &tx,
                        &self.session,
                        &id,
                        writer,
                        now,
                        fact,
                        &mut [],
                    )
                    .map_err(write)?;
                }
                Terminal::Failed(fact) => {
                    grapt_store::step_failed::record(
                        &tx,
                        &self.session,
                        &id,
                        writer,
                        now,
                        fact,
                        &mut [],
                    )
                    .map_err(write)?;
                }
            }
            tx.commit()
                .map_err(|source| OwnerError {
                    reason: source.to_string(),
                })?
                .discard();
        }
        self.settled = true;
        match capture_failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl<A: ArtifactStore, S: CommittedFactSink> DurableAttemptOwner<'static, A, S> {
    /// Production composition with owned authorities and the one Inbox-owned
    /// store. Owning the adapters lets a Location return this owner without a
    /// self-borrow escaping `open_chain`.
    #[allow(clippy::too_many_arguments)]
    pub fn elect_inbox_owned(
        inbox: Arc<Mutex<Inbox>>,
        artifacts: A,
        clock: Arc<dyn ClockAuthority + Send + Sync>,
        ids: Arc<dyn IdAuthority + Send + Sync>,
        sink: S,
        session: grapt_types::SessionId,
        step: StepNumber,
        resume: Option<ResumedMessage>,
    ) -> Result<Self, DurableOwnerError> {
        Self::elect_over(
            DurableStore::Inbox(inbox),
            artifacts,
            ClockCustody::Owned(clock),
            IdCustody::Owned(ids),
            sink,
            session,
            step,
            resume,
        )
    }
}

enum Terminal {
    Ended(StepEndedFact),
    Failed(StepFailedFact),
}

impl<A: ArtifactStore, S: CommittedFactSink> AttemptOwner for DurableAttemptOwner<'_, A, S> {
    fn timing_context(&self) -> Option<grapt_types::timing::TimingContext> {
        self.timing
            .as_ref()
            .map(|observer| grapt_types::timing::TimingContext {
                observer: Arc::clone(observer),
                scope: grapt_types::timing::TimingScope {
                    session: Some(self.session.clone()),
                    attempt: self.attempt.clone(),
                    class: grapt_types::timing::TimingClass::Other,
                },
            })
    }

    fn begin_step(&mut self, step: StepNumber) -> Result<(), OwnerError> {
        if step == self.step {
            return Ok(());
        }
        if !self.settled
            || !self.blocks.is_empty()
            || self.step.value().checked_add(1) != Some(step.value())
        {
            return Err(OwnerError {
                reason: "next Step arrived before the previous owner settled".into(),
            });
        }
        self.step = step;
        self.message = None;
        self.attempt = None;
        self.continuation = None;
        self.next_ordinal = ContentOrdinal::FIRST;
        self.settled = false;
        self.sink.step_began(step);
        Ok(())
    }

    fn committed_output(&self) -> bool {
        self.attempt_visible
    }

    fn prepare_step_terminal(&mut self) -> Result<(), OwnerError> {
        let mut first_failure = self.commit_held().err();
        while let Some((block, _)) = self.blocks.first() {
            if let Err(error) = self.close_block(*block) {
                first_failure.get_or_insert(error);
            }
        }
        match first_failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn step_terminal_committed(
        &mut self,
        fact: &DurableFact,
        position: &CommittedPosition,
        publication: crate::owner::TerminalPublication,
    ) {
        self.settled = true;
        if publication == crate::owner::TerminalPublication::Notify {
            self.sink.step_settled(position, fact);
            self.sink.offer(fact);
        }
    }

    fn capture_hosted_output(
        &mut self,
        text: &str,
    ) -> Result<grapt_types::ToolTerminalArtifact, OwnerError> {
        self.capture_hosted(text)
    }

    fn commit(&mut self, fact: DurableFact) -> Result<(), OwnerError> {
        if self.settled
            && !matches!(
                fact,
                DurableFact::StepEnded { .. } | DurableFact::StepFailed { .. }
            )
        {
            return Err(OwnerError {
                reason: "a settled Step cannot accept new attempt content".into(),
            });
        }
        match &fact {
            DurableFact::AttemptStarted { attempt } => {
                // A resumed Step admits exactly the attempt its continuation
                // authority named. Accepting whatever arrived — as an earlier
                // revision did — meant the Step's message silently continued
                // under a second physical attempt, with the swap recorded
                // nowhere: durable history showed one message and gave no way
                // to tell which attempt produced which half of it.
                if let Some(authorized) = &self.continuation {
                    if attempt != authorized {
                        return Err(OwnerError {
                            reason: format!(
                                "step {} was resumed for attempt {}, but attempt {} started; \
                                 a continuation is authorized for ONE attempt",
                                self.step.value(),
                                authorized.as_str(),
                                attempt.as_str()
                            ),
                        });
                    }
                }
                self.open_message(attempt)?;
                self.attempt = Some(attempt.clone());
                self.attempt_visible = false;
                self.retryable_failure = false;
                Ok(())
            }
            DurableFact::Fragment { fragment } => {
                let stage = match fragment.as_ref() {
                    AttemptFragment::TextDelta { .. } => {
                        Some(grapt_types::timing::TimingStage::LiveText)
                    }
                    AttemptFragment::TextClosed { .. } => {
                        Some(grapt_types::timing::TimingStage::TextClosed)
                    }
                    AttemptFragment::ProviderTerminal { .. } => {
                        Some(grapt_types::timing::TimingStage::ProviderTerminal)
                    }
                    _ => None,
                };
                if let Some(stage) = stage {
                    let _text = self.timing_context().map(|context| context.span(stage));
                }
                self.attempt_visible |= fragment.is_visible_output();
                self.retryable_failure |= fragment.may_retry();
                self.apply_fragment(fragment)
            }
            DurableFact::NestedToolCalled {
                parent,
                call,
                tool,
                catalog,
                input,
            } => self.append_nested(parent, call, tool, catalog, input),
            DurableFact::ToolTerminal { call, outcome } => self.append_tool_terminal(call, outcome),
            // Attempt terminals belong to the operational attempt receipt, not
            // to the Session's transcript.
            DurableFact::AttemptEnded { .. } => Ok(()),
            // An unfinished block is never durable when the Step retries; the retry rebuilds from the same history.
            // An interrupt or a Step-ending failure keeps it; the Step terminal
            // closes it into the failed Step's truth.
            DurableFact::AttemptFailed { failure } => {
                if self.retries(failure) {
                    self.blocks.clear();
                    self.held.clear();
                    self.held_bytes = 0;
                    self.sink.open_blocks_discarded();
                }
                Ok(())
            }
            DurableFact::StepEnded { finish } => {
                let completion = match finish.normalize() {
                    NormalizedFinish::Completed { completion } => completion,
                    // A finish that normalizes to a FAILURE is not a Step
                    // success, whatever fact the driver reached for. Refusing
                    // is the only truthful answer: writing it as ended would
                    // publish a content-policy refusal as a clean completion.
                    NormalizedFinish::Failed { .. } => {
                        return Err(OwnerError {
                            reason: format!(
                                "provider finish {finish:?} normalizes to a failure and cannot \
                                 be written as a Step success"
                            ),
                        })
                    }
                };
                self.settle(Terminal::Ended(StepEndedFact::new(self.step, completion)))
            }
            DurableFact::StepFailed { failure } => {
                // The detail is in hand HERE and the durable fact drops it, so
                // this is the last place it can be read.
                crate::failure_diagnostic::step_failed(&self.session, self.step, failure);
                let fact =
                    StepFailedFact::new(self.step, failure.reason().clone()).map_err(|source| {
                        OwnerError {
                            reason: source.to_string(),
                        }
                    })?;
                self.settle(Terminal::Failed(fact))
            }
        }
        .map(|()| {
            self.sink.offer(&fact);
        })
    }

    fn tool_terminal_committed(&mut self, fact: &DurableFact) {
        self.sink.offer(fact);
    }

    fn notify(&mut self, _fact: &DurableFact) {}

    fn observe(&mut self, _observation: Observation) {}
}

fn write(source: WriteError) -> OwnerError {
    OwnerError {
        reason: source.to_string(),
    }
}

/// Takes the store lock, recovering a poisoned one.
///
/// A poisoned mutex means another thread panicked while holding it. The store
/// itself is transaction-protected — an aborted transaction rolled back — so
/// the DATA is not in doubt, and refusing every subsequent commit because a
/// sibling panicked would turn one failure into a permanently dead Session.
fn lock_event(store: &Arc<Mutex<EventStore>>) -> std::sync::MutexGuard<'_, EventStore> {
    match store.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests;
