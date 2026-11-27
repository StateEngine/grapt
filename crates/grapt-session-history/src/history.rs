//! The Session's message history: a pure deterministic fold over durable
//! facts.
//!
//! Law: `SECODE_RUST_RECONSTRUCTION_DISPOSITION_2026-08-07.md` row
//! *Session projection/history* — "Pure deterministic fold and canonical
//! exporter; no live transport state in projection"; the ordered-content row
//! — "One `NormalizedContentSequence` with exact `ContentOrdinal` preserves
//! text, reasoning/signature, tool and opaque-item position through
//! replay/truncation"; and the provider-stream contract rule 4 — "A Step has
//! one and only one durable terminal fact".
//!
//! # Why a fold and not a projection table
//!
//! The session LIST is a durable projection because it is queried across
//! sessions with a keyset cursor and cannot be rebuilt per request. One
//! Session's history is the opposite shape: it is read for one aggregate,
//! bounded by that aggregate's own log, and its correctness claim is precisely
//! that a restart reproduces it. A durable table would make that claim
//! untestable — it would prove the table survived a restart, which is not the
//! same statement. Folding from the log means restart equality is the fold
//! being a function, and a witness can assert it by folding twice.
//!
//! # Three refusals that are NOT "empty history"
//!
//! A read that cannot be answered truthfully says so:
//!
//! * two content items claiming one ordinal is corruption, not a last-write
//!   wins;
//! * a Step with two durable terminals is the resurrection this vertical
//!   exists to make impossible — reporting the second as an update would hide
//!   exactly the defect;
//! * a content item whose ordinals do not form a dense sequence from
//!   [`ContentOrdinal::FIRST`] cannot build a
//!   [`NormalizedContentSequence`], and inventing a sparse one here would put
//!   a shape in the read model that the type refuses.
//!
//! An item for a message that was never opened is DIFFERENT and is counted,
//! not refused: head compaction legitimately removes an opening fact while its
//! items remain, exactly as [`crate::projection`] measures `seedless` rows.

use grapt_event::session_message;
use grapt_event::{session_step_ended, session_step_failed};
use grapt_types::{
    ArtifactRef, CaptureDisposition, ContentKind, ContentOrdinal, EventId, MessageId,
    NormalizedContentItem, NormalizedContentSequence, PhysicalAttemptId,
    ProviderCompletionDisposition, RecordedAttemptUsage, SessionId, StepNumber, StepTerminalReason,
    ToolCallId,
};
use serde_json::Value;
use std::collections::HashMap;

/// One Step's durable terminal, as the fold read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepTerminal {
    Ended {
        completion: ProviderCompletionDisposition,
    },
    Failed {
        reason: StepTerminalReason,
    },
}

/// Exact durable coordinates carried by the typed Session projection.
///
/// Product/API code consumes this value instead of querying the event table
/// for `MAX(seq)` or the newest mutable row. It is optional only on the pure
/// in-memory [`MessageHistory::apply`] seam used by language-neutral fold
/// fixtures; reads from [`SessionHistoryStore`] always populate it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedPosition {
    pub event: EventId,
    pub seq: u64,
    pub created_ms: i64,
}

/// One durable content item, with the disposition of its capture.
///
/// The two travel together in ONE ordered vector. Holding the dispositions in a
/// second array indexed by position would be the parallel-per-kind-array shape
/// the supersession matrix corrects, one level down: the arrays could drift and
/// nothing in the type would notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedContentItem {
    pub item: NormalizedContentItem,
    /// Whether the referenced bytes are the whole value or a bounded prefix.
    /// A projection that presented a truncated capture as the whole value is
    /// the defect this exists to prevent.
    pub disposition: CaptureDisposition,
    /// The commit that appended this exact item. Never inferred from the
    /// message opening or from whatever row happens to be newest later.
    pub committed: Option<CommittedPosition>,
}

/// Maximum durable tool-call rows one continuation read owns (SC-REL-03).
///
/// The query observes one extra row so the first over-bound call is evidence,
/// never an inference from a full page.
///
/// Raised from 128 so that a coding Session compacts on tokens, not on its
/// call count (SC-COMPACT-01); the product's structural probe mirrors it.
pub const MAX_TOOL_CALL_PAGE_ROWS: usize = grapt_store::session_event_queries::batch::MAX_ROWS;

/// One tool call decoded by the Session-owned bounded read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableToolCall {
    message: MessageId,
    call: ToolCallId,
    payload: ArtifactRef,
    disposition: CaptureDisposition,
    committed: CommittedPosition,
}

impl DurableToolCall {
    pub fn message(&self) -> &MessageId {
        &self.message
    }

    pub fn call(&self) -> &ToolCallId {
        &self.call
    }

    pub fn payload(&self) -> &ArtifactRef {
        &self.payload
    }

    pub fn disposition(&self) -> CaptureDisposition {
        self.disposition
    }

    pub fn committed(&self) -> &CommittedPosition {
        &self.committed
    }
}

/// Complete bounded page of durable tool calls for one Session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionToolCallPage {
    rows: Vec<DurableToolCall>,
}

impl SessionToolCallPage {
    pub fn rows(&self) -> &[DurableToolCall] {
        &self.rows
    }
}

/// One message's ordered content.
///
/// Built by one constructor from one sorted vector, so the items and the
/// validated sequence cannot disagree about order or length.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageContent {
    items: Vec<CapturedContentItem>,
    sequence: NormalizedContentSequence,
}

impl MessageContent {
    /// Refuses unless the ordinals form a dense sequence from
    /// [`ContentOrdinal::FIRST`] — the check belongs to
    /// [`NormalizedContentSequence`], and this type does not get a second
    /// opinion about it.
    fn new(items: Vec<CapturedContentItem>) -> Option<Self> {
        let sequence =
            NormalizedContentSequence::new(items.iter().map(|held| held.item.clone()).collect())?;
        Some(Self { items, sequence })
    }

    /// Every item in ordinal order, each with its capture disposition.
    pub fn items(&self) -> &[CapturedContentItem] {
        &self.items
    }

    /// The ratified ordered sequence, for a consumer that needs exactly that.
    pub fn sequence(&self) -> &NormalizedContentSequence {
        &self.sequence
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// True when every item's bytes are complete within their capture bound.
    ///
    /// Carried as a question a caller can ask rather than left to be derived,
    /// because "was any of this truncated" is the question a transcript reader
    /// actually has, and deriving it at each call site is where one site
    /// forgets.
    pub fn is_complete(&self) -> bool {
        self.items.iter().all(|held| held.disposition.is_complete())
    }
}

/// One assistant message, with its ordered content.
///
/// `content` is `None` while the message has no items at all — an opened
/// message that produced nothing is a real state and is not the same as a
/// message with an empty sequence, which [`NormalizedContentSequence`] does
/// not represent.
#[derive(Debug, Clone, PartialEq)]
pub struct AssistantMessage {
    pub selection: Option<session_message::AssistantSelection>,
    pub message: MessageId,
    pub attempt: PhysicalAttemptId,
    pub step: StepNumber,
    /// Position in the durable log, so a caller can page without inventing an
    /// order the fold did not observe.
    pub opened_at_seq: u64,
    /// Exact opening commit when read from the durable store.
    pub opened: Option<CommittedPosition>,
    pub content: Option<MessageContent>,
    /// The Step's one terminal, once it has settled. `None` is an unsettled
    /// Step, never a success.
    pub terminal: Option<StepTerminal>,
    /// Exact Step-terminal commit, separate from any later admitted input.
    pub terminal_committed: Option<CommittedPosition>,
    /// Actual v3 settlement observation, separate from reserved event identity.
    pub completed_at: Option<grapt_event::session_step_observed::CompletionObservedAt>,
    /// The attempt whose blocks last contributed to this message (the opening
    /// attempt if no block exists). Distinct from the original opening attempt
    /// when a pre-visibility retry reused the Step's message identity.
    pub usage_attempt: PhysicalAttemptId,
    /// Every typed observation for `usage_attempt`, in
    /// durable order. Absent fields remain absent; callers decide how later
    /// phases reconcile rather than asking product SQL to coerce them to zero.
    pub usage: Vec<RecordedAttemptUsage>,
    /// Latest timestamp among this message's typed opening/content/usage/
    /// terminal facts. `None` on pure fold fixtures with no commit metadata.
    pub updated_ms: Option<i64>,
    /// Latest exact commit among this message's typed facts. This deliberately
    /// excludes later input admission: product interruption projection can
    /// bind to the interrupted attempt without asking for the Session's
    /// mutable newest row.
    pub updated: Option<CommittedPosition>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HistoryError {
    /// A row this fold owns did not decode.
    ///
    /// The reason is an owned string rather than a nested payload error
    /// because this fold reads THREE payload contracts, each with its own
    /// error type. Carrying all three as separate variants would make a
    /// consumer match on which crate spelled the failure; carrying one as a
    /// `&'static str` would require leaking the other two's messages. The row
    /// coordinates — which kind, which sequence — are the part a reader acts
    /// on, and they stay typed.
    #[error("{kind} at seq {seq} has an undecodable payload: {reason}")]
    Payload {
        kind: &'static str,
        seq: u64,
        reason: String,
    },
    /// Two facts that cannot both be true of one transcript: an attempt bound
    /// to two messages, a completion observed before its message opened. This
    /// is not a decoder failure, so it keeps refusing the read instead of
    /// degrading to an omission.
    #[error("{kind} at seq {seq} contradicts the folded transcript: {reason}")]
    Contradiction {
        kind: &'static str,
        seq: u64,
        reason: String,
    },
    #[error("message {message} has two content items at ordinal {ordinal}")]
    DuplicateOrdinal { message: String, ordinal: u64 },
    #[error(
        "message {message} content ordinals are not a dense sequence from zero; \
         the log cannot be read as one ordered content sequence"
    )]
    SparseContent { message: String },
    #[error(
        "step {step} has a second durable terminal at seq {seq}: a Step has one \
         and only one"
    )]
    ResurrectedTerminal { step: u32, seq: u64 },
    #[error("message {message} was opened twice; the second opening is at seq {seq}")]
    ReopenedMessage { message: String, seq: u64 },
}

/// One durable row, reduced to what the fold consumes.
///
/// Borrowed rather than owned: the fold does not need the event's canonical
/// bytes, its aggregate or its writer epoch, and taking them would let a caller
/// believe this type is the durable event.
#[derive(Debug, Clone, Copy)]
pub struct HistoryRow<'a> {
    pub seq: u64,
    pub kind_name: &'a str,
    pub kind_version: u32,
    pub data: &'a Value,
}

/// The accumulating fold.
///
/// `Default` is deliberate and safe: an empty fold is an empty history, which
/// is the truthful answer for a Session that has produced nothing. That is not
/// the case for a durable OWNER, which must not have a zero-argument
/// constructor — this type writes nothing.
#[derive(Debug, Default)]
pub struct MessageHistory {
    order: Vec<MessageId>,
    messages: HashMap<MessageId, Opened>,
    items: HashMap<MessageId, Vec<CapturedContentItem>>,
    terminals: HashMap<u32, StepTerminal>,
    terminal_positions: HashMap<u32, CommittedPosition>,
    completion_observations: HashMap<u32, grapt_event::session_step_observed::CompletionObservedAt>,
    /// Content items whose opening fact is not in the folded range. A measured
    /// fact about head compaction, not a failure — see the module docs.
    orphaned_items: u64,
    /// Rows or replay steps this fold could not read and therefore left out,
    /// in sequence order. A shorter transcript must never be reported as a
    /// complete one, so each omission is carried and surfaced. See
    /// [`HistoryOmission`].
    omissions: Vec<HistoryOmission>,
    /// Usage observations, latest phase last, keyed by attempt. Kept in
    /// observation order so a reconciliation is visibly a LATER fact rather
    /// than an overwrite of an earlier one.
    /// Keyed by the OWNER type, never by its text. `PhysicalAttemptId` is
    /// hashable, so a `String` key bought nothing and cost the property the
    /// project enforces mechanically: an attempt identity that has been
    /// flattened to text can be built from any string, and the read model then
    /// has a second, unvalidated way to name an attempt.
    usage: HashMap<PhysicalAttemptId, Vec<RecordedAttemptUsage>>,
    usage_positions: HashMap<PhysicalAttemptId, Vec<CommittedPosition>>,
    // Explicit durable message/attempt bindings, in first-observed order.
    // A pre-visibility retry can open a block under the existing Step message.
    message_attempts: HashMap<MessageId, Vec<PhysicalAttemptId>>,
    attempt_messages: HashMap<PhysicalAttemptId, MessageId>,
    /// The message that carries each model-visible tool call, so a tool
    /// terminal can bind a retry that produced only tool calls.
    call_messages: HashMap<ToolCallId, MessageId>,
}

#[derive(Debug, Clone)]
struct Opened {
    selection: Option<session_message::AssistantSelection>,
    attempt: PhysicalAttemptId,
    step: StepNumber,
    seq: u64,
    committed: Option<CommittedPosition>,
}

impl MessageHistory {
    /// Folds one durable row.
    ///
    /// Rows this history does not own return `Ok(())` untouched: the log is one
    /// family-tagged union and the inbox, permission, form and campaign
    /// clusters are not this fold's business.
    pub fn apply(&mut self, row: HistoryRow<'_>) -> Result<(), HistoryError> {
        self.apply_committed(row, None)
    }

    fn apply_committed(
        &mut self,
        row: HistoryRow<'_>,
        committed: Option<CommittedPosition>,
    ) -> Result<(), HistoryError> {
        let (seq, kind_name, data) = (row.seq, row.kind_name, row.data);
        let applied = match (row.kind_name, row.kind_version) {
            ("session.assistant-message.opened", 1 | 2) => self.open(row, committed),
            ("session.content-block.opened", 1) => self.open_block(row),
            ("session.content.appended", 1) => self.append(row, committed),
            ("session.usage.observed", 1) => self.observe(row, committed, true),
            ("session.usage.observed", 2) => self.observe(row, committed, false),
            ("session.step.ended", 2 | 3) => self.end(row, committed),
            ("session.step.failed", 2 | 3) => self.fail(row, committed),
            // Binding evidence, not an owned kind.
            (name, 1) if store::BINDING_EVIDENCE_KINDS.contains(&name) => {
                self.bind_tool_attempt(row)
            }
            // Every other (name, version) pair — including an owned name at a
            // version this fold does not read, which is how the ORACLE's v1
            // spellings arrive — is not this fold's business. Refusing those
            // would break a migrated history that legitimately carries them.
            _ => return Ok(()),
        };
        // A row this fold OWNS but cannot decode costs that one item and is
        // recorded, never the whole transcript: the writer is allowed to be
        // wrong and the reader must still answer. A structural contradiction is
        // a different fact and keeps refusing — two items at one ordinal, a
        // reopened message or a resurrected Step terminal says the log as a
        // whole cannot be read as this history, and degrading it would hide
        // exactly the defect the fold exists to catch.
        match applied {
            Err(error) if matches!(error, HistoryError::Payload { .. }) => {
                self.record_unreadable(seq, kind_name, data, &error);
                Ok(())
            }
            other => other,
        }
    }

    fn open(
        &mut self,
        row: HistoryRow<'_>,
        committed: Option<CommittedPosition>,
    ) -> Result<(), HistoryError> {
        let fact = match row.kind_version {
            2 => session_message::decode_opened_v2(row.data),
            _ => session_message::decode_opened(row.data),
        }
        .map_err(|source| HistoryError::Payload {
            kind: "session.assistant-message.opened.1",
            seq: row.seq,
            reason: source.to_string(),
        })?;
        let message = fact.message().clone();
        if self.messages.contains_key(&message) {
            return Err(HistoryError::ReopenedMessage {
                message: message.as_str().to_owned(),
                seq: row.seq,
            });
        }
        self.bind_attempt(&message, fact.attempt(), row.seq)?;
        self.order.push(message.clone());
        self.messages.insert(
            message,
            Opened {
                selection: fact.selection().cloned(),
                attempt: fact.attempt().clone(),
                step: fact.step(),
                seq: row.seq,
                committed,
            },
        );
        Ok(())
    }

    fn bind_attempt(
        &mut self,
        message: &MessageId,
        attempt: &PhysicalAttemptId,
        seq: u64,
    ) -> Result<(), HistoryError> {
        if self
            .attempt_messages
            .get(attempt)
            .is_some_and(|prior| prior != message)
        {
            return Err(HistoryError::Contradiction {
                kind: "session.content-block.opened",
                seq,
                reason: "one Physical Attempt cannot belong to two assistant messages".into(),
            });
        }
        self.attempt_messages
            .insert(attempt.clone(), message.clone());
        let attempts = self.message_attempts.entry(message.clone()).or_default();
        if !attempts.contains(attempt) {
            attempts.push(attempt.clone());
        }
        Ok(())
    }

    /// A pre-visibility retry that produces only tool calls opens no content
    /// block, so its tool terminals are the durable evidence that the retried
    /// attempt belongs to the Step's message — and that its usage is the
    /// message's. Only a call already appended to a message binds; nested
    /// CodeMode calls and summary attempts never do. An attempt already bound to
    /// another message is left alone. A corrupt terminal is absent evidence
    /// for every reader (`StoredTerminal`): here it leaves usage absent, not wrong.
    fn bind_tool_attempt(&mut self, row: HistoryRow<'_>) -> Result<(), HistoryError> {
        let Ok(fact) = grapt_event::session_tool_terminal::decode(row.data) else {
            return Ok(());
        };
        let Some(message) = self.call_messages.get(fact.call()).cloned() else {
            return Ok(());
        };
        if self
            .attempt_messages
            .get(fact.attempt())
            .is_some_and(|bound| bound != &message)
        {
            return Ok(());
        }
        self.bind_attempt(&message, fact.attempt(), row.seq)
    }

    fn open_block(&mut self, row: HistoryRow<'_>) -> Result<(), HistoryError> {
        let fact = session_message::decode_block_opened(row.data).map_err(|source| {
            HistoryError::Payload {
                kind: "session.content-block.opened.1",
                seq: row.seq,
                reason: source.to_string(),
            }
        })?;
        self.bind_attempt(fact.message(), fact.attempt(), row.seq)
    }

    fn append(
        &mut self,
        row: HistoryRow<'_>,
        committed: Option<CommittedPosition>,
    ) -> Result<(), HistoryError> {
        let fact =
            session_message::decode_appended(row.data).map_err(|source| HistoryError::Payload {
                kind: "session.content.appended.1",
                seq: row.seq,
                reason: source.to_string(),
            })?;
        let message = fact.message().clone();
        if !self.messages.contains_key(&message) {
            self.orphaned_items = self.orphaned_items.saturating_add(1);
        }
        if let ContentKind::Tool { call } = fact.item().kind() {
            self.call_messages.insert(call.clone(), message.clone());
        }
        let ordinal = fact.item().ordinal();
        let items = self.items.entry(message.clone()).or_default();
        if items.iter().any(|held| held.item.ordinal() == ordinal) {
            return Err(HistoryError::DuplicateOrdinal {
                message: message.as_str().to_owned(),
                ordinal: ordinal.value(),
            });
        }
        items.push(CapturedContentItem {
            item: fact.item().clone(),
            disposition: fact.disposition(),
            committed,
        });
        Ok(())
    }

    fn observe(
        &mut self,
        row: HistoryRow<'_>,
        committed: Option<CommittedPosition>,
        legacy_v1: bool,
    ) -> Result<(), HistoryError> {
        let (attempt, usage) = if legacy_v1 {
            let fact = session_message::decode_observed_v1(row.data).map_err(|source| {
                HistoryError::Payload {
                    kind: "session.usage.observed.1",
                    seq: row.seq,
                    reason: source.to_string(),
                }
            })?;
            (
                fact.attempt().clone(),
                RecordedAttemptUsage::LegacyV1(fact.usage().clone()),
            )
        } else {
            let fact = session_message::decode_observed(row.data).map_err(|source| {
                HistoryError::Payload {
                    kind: "session.usage.observed.2",
                    seq: row.seq,
                    reason: source.to_string(),
                }
            })?;
            (
                fact.attempt().clone(),
                RecordedAttemptUsage::V2(fact.usage().clone()),
            )
        };
        self.usage.entry(attempt.clone()).or_default().push(usage);
        if let Some(committed) = committed {
            self.usage_positions
                .entry(attempt)
                .or_default()
                .push(committed);
        }
        Ok(())
    }

    fn end(
        &mut self,
        row: HistoryRow<'_>,
        committed: Option<CommittedPosition>,
    ) -> Result<(), HistoryError> {
        let (fact, completed_at) = if row.kind_version == 3 {
            let observed =
                grapt_event::session_step_observed::decode_ended(row.data).map_err(|source| {
                    HistoryError::Payload {
                        kind: "session.step.ended.3",
                        seq: row.seq,
                        reason: source.to_string(),
                    }
                })?;
            (*observed.fact(), Some(observed.completed_at()))
        } else {
            let fact =
                session_step_ended::decode(row.data).map_err(|source| HistoryError::Payload {
                    kind: "session.step.ended.2",
                    seq: row.seq,
                    reason: source.to_string(),
                })?;
            (fact, None)
        };
        self.settle(
            fact.step(),
            StepTerminal::Ended {
                completion: fact.completion(),
            },
            row.seq,
            committed,
            completed_at,
        )
    }

    fn fail(
        &mut self,
        row: HistoryRow<'_>,
        committed: Option<CommittedPosition>,
    ) -> Result<(), HistoryError> {
        let (fact, completed_at) = if row.kind_version == 3 {
            let observed =
                grapt_event::session_step_observed::decode_failed(row.data).map_err(|source| {
                    HistoryError::Payload {
                        kind: "session.step.failed.3",
                        seq: row.seq,
                        reason: source.to_string(),
                    }
                })?;
            (observed.fact().clone(), Some(observed.completed_at()))
        } else {
            let fact =
                session_step_failed::decode(row.data).map_err(|source| HistoryError::Payload {
                    kind: "session.step.failed.2",
                    seq: row.seq,
                    reason: source.to_string(),
                })?;
            (fact, None)
        };
        self.settle(
            fact.step(),
            StepTerminal::Failed {
                reason: fact.reason().clone(),
            },
            row.seq,
            committed,
            completed_at,
        )
    }

    /// A Step settles ONCE.
    ///
    /// The second terminal is refused even when it is byte-identical to the
    /// first. An idempotent RETRY never reaches here — the store reconciles it
    /// at the append chokepoint and it appears in the log exactly once — so a
    /// second row at a different sequence is a second FACT, and a fold that
    /// tolerated it would report a resurrected Step as ordinary history.
    fn settle(
        &mut self,
        step: StepNumber,
        terminal: StepTerminal,
        seq: u64,
        committed: Option<CommittedPosition>,
        completed_at: Option<grapt_event::session_step_observed::CompletionObservedAt>,
    ) -> Result<(), HistoryError> {
        if self.terminals.contains_key(&step.value()) {
            return Err(HistoryError::ResurrectedTerminal {
                step: step.value(),
                seq,
            });
        }
        if let Some(at) = completed_at {
            if self.messages.values().any(|opened| {
                opened.step == step
                    && opened
                        .committed
                        .as_ref()
                        .is_some_and(|position| at.as_millis() < position.created_ms)
            }) {
                return Err(HistoryError::Contradiction {
                    kind: "session.step.completion",
                    seq,
                    reason: "completion observation precedes assistant opening".into(),
                });
            }
            self.completion_observations.insert(step.value(), at);
        }
        self.terminals.insert(step.value(), terminal);
        if let Some(committed) = committed {
            self.terminal_positions.insert(step.value(), committed);
        }
        Ok(())
    }

    /// Content items whose opening fact was outside the folded range.
    pub fn orphaned_items(&self) -> u64 {
        self.orphaned_items
    }

    /// Every usage observation for one attempt, in observation order.
    pub fn usage_for(&self, attempt: &PhysicalAttemptId) -> &[RecordedAttemptUsage] {
        match self.usage.get(attempt) {
            Some(observations) => observations,
            None => &[],
        }
    }

    /// Session accounting includes every recorded attempt, including failed
    /// requests that produced no block and therefore have no visible message.
    /// Reconcile each attempt independently; phases of distinct attempts must
    /// never overwrite one another's billable counts.
    pub fn usage_attempts(
        &self,
    ) -> impl Iterator<Item = (&PhysicalAttemptId, &[RecordedAttemptUsage])> {
        self.usage
            .iter()
            .map(|(attempt, observations)| (attempt, observations.as_slice()))
    }

    fn message_usage_attempt(&self, message: &MessageId) -> Option<&PhysicalAttemptId> {
        self.message_attempts
            .get(message)
            .and_then(|attempts| attempts.last())
    }

    fn message_usage_positions(
        &self,
        message: &MessageId,
    ) -> impl Iterator<Item = &CommittedPosition> {
        self.message_attempts
            .get(message)
            .into_iter()
            .flatten()
            .flat_map(|attempt| self.usage_positions.get(attempt).into_iter().flatten())
    }

    /// The newest typed Step-terminal commit, independent of whether that Step
    /// opened an assistant message.
    ///
    /// A pre-first-fragment failure has a durable `session.step.failed.2` fact
    /// but no `session.assistant-message.opened.1` fact. Consumers that walk
    /// [`Self::messages`] cannot see that terminal and are tempted to fall back
    /// to an older admission position. Keeping terminal positions on the
    /// Session fold makes absence truthful without coupling terminal identity
    /// to provider content.
    pub fn latest_terminal_committed(&self) -> Option<&CommittedPosition> {
        self.terminal_positions
            .values()
            .max_by_key(|committed| committed.seq)
    }

    /// The folded history, in the order the messages were opened.
    ///
    /// Building the ordered sequences happens HERE rather than during the fold
    /// because an ordinal gap is only knowable once every item has arrived: a
    /// fold that refused a gap as it saw one would refuse every out-of-order
    /// arrival, and the log does not promise the store's append order matches
    /// the ordinal order for items written in one transaction.
    pub fn messages(&self) -> Result<Vec<AssistantMessage>, HistoryError> {
        let mut out = Vec::with_capacity(self.order.len());
        for message in &self.order {
            let opened = match self.messages.get(message) {
                Some(opened) => opened,
                // Structurally unreachable: `order` and `messages` are written
                // together in `open`. Represented rather than unwrapped.
                None => continue,
            };
            let content = match self.items.get(message) {
                None => None,
                Some(items) => {
                    let mut items = items.clone();
                    items.sort_by_key(|held| held.item.ordinal().value());
                    Some(
                        MessageContent::new(items).ok_or_else(|| HistoryError::SparseContent {
                            message: message.as_str().to_owned(),
                        })?,
                    )
                }
            };
            let updated = self.message_updated(opened, message);
            let usage_attempt = self
                .message_usage_attempt(message)
                .unwrap_or(&opened.attempt);
            out.push(AssistantMessage {
                selection: opened.selection.clone(),
                message: message.clone(),
                attempt: opened.attempt.clone(),
                step: opened.step,
                opened_at_seq: opened.seq,
                opened: opened.committed.clone(),
                content,
                terminal: self.terminals.get(&opened.step.value()).cloned(),
                terminal_committed: self.terminal_positions.get(&opened.step.value()).cloned(),
                completed_at: self
                    .completion_observations
                    .get(&opened.step.value())
                    .copied(),
                usage_attempt: usage_attempt.clone(),
                usage: self.usage.get(usage_attempt).cloned().unwrap_or_default(),
                updated_ms: self.message_updated_ms(opened, message),
                updated,
            });
        }
        Ok(out)
    }

    fn message_updated(&self, opened: &Opened, message: &MessageId) -> Option<CommittedPosition> {
        let opening = opened.committed.iter();
        let content = self
            .items
            .get(message)
            .into_iter()
            .flatten()
            .filter_map(|held| held.committed.as_ref());
        let usage = self.message_usage_positions(message);
        let terminal = self
            .terminal_positions
            .get(&opened.step.value())
            .into_iter();
        opening
            .chain(content)
            .chain(usage)
            .chain(terminal)
            .max_by_key(|committed| committed.seq)
            .cloned()
    }

    fn message_updated_ms(&self, opened: &Opened, message: &MessageId) -> Option<i64> {
        let opening = opened.committed.as_ref().map(|held| held.created_ms);
        let content = self
            .items
            .get(message)
            .into_iter()
            .flatten()
            .filter_map(|held| {
                held.committed
                    .as_ref()
                    .map(|committed| committed.created_ms)
            });
        let usage = self
            .message_usage_positions(message)
            .map(|committed| committed.created_ms);
        let terminal = self
            .terminal_positions
            .get(&opened.step.value())
            .map(|committed| committed.created_ms);
        opening
            .into_iter()
            .chain(content)
            .chain(usage)
            .chain(terminal)
            .max()
    }

    /// The first ordinal a new content item for this message may claim.
    ///
    /// Used by the durable owner to stamp the next item. Derived from what is
    /// DURABLE rather than from a counter the owner keeps, so a restarted
    /// owner resumes at the true position instead of overwriting ordinal zero.
    pub fn next_ordinal(&self, message: &MessageId) -> ContentOrdinal {
        let highest = self
            .items
            .get(message)
            .and_then(|items| items.iter().map(|held| held.item.ordinal().value()).max());
        match highest {
            None => ContentOrdinal::FIRST,
            Some(value) => ContentOrdinal::new(value.saturating_add(1)),
        }
    }
}

/// The safety bound on durable rows one history read folds (SC-REL-03).
///
/// A Session's log grows without limit, and a read that folded all of it would
/// make `history` cost whatever the conversation has cost so far. The page is
/// bounded in ROWS; the bytes inside each row are already bounded by the
/// artifact contract, because content items carry references rather than
/// payloads.
pub const MAX_HISTORY_ROWS: usize = 50_000;

/// The bound on the row data one history read holds, summed over its rows and
/// checked as each row is read, so a window of valid but large rows is a
/// typed refusal before it is held rather than after (SC-COMPACT-01).
pub const MAX_HISTORY_DATA_BYTES: usize = 64 * 1024 * 1024;

/// The bounded durable read that feeds [`MessageHistory`].
///
/// It lives here, beside the fold, rather than in the runtime crate that calls
/// it. The runtime owns the session supervisor, the inbox drain and step
/// scheduling — the crate-graph amendment says so in those words — and a SQL
/// statement against the Session aggregate in that crate would be a Session
/// read written outside the crate that owns Session reads.
#[derive(Debug)]
pub struct SessionHistoryStore<'a> {
    conn: &'a rusqlite::Connection,
}

mod omission;
mod store;
pub use omission::{HistoryOmission, HistoryOmissionClass};
pub use store::HistoryReadError;
pub use store::OWNED_KINDS;
pub mod accounting;

#[cfg(test)]
mod tests;
