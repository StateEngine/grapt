//! Write-side Session inbox: durable admission and promotion application services over
//! `grapt_store::EventStore`.
//!
//! Laws:
//! - EVERY admission decision — reconciliation reads included — happens
//!   inside the SAME immediate transaction as the append+projection
//!   (review A): two contending callers serialize at BEGIN IMMEDIATE and
//!   the loser deterministically observes the winner's row.
//! - `resume: false` is admit-only; the typed `WakeDisposition` is advisory
//!   and nothing here schedules execution.
//! - MessageId exact retry reconciles ONLY when session, text, and delivery
//!   all match. After promotion the durable admitted event AND the projected
//!   user message are BOTH strictly decoded and corroborated; a missing,
//!   duplicate, corrupt, or mismatched record is typed `Corruption`, never a
//!   reconciliation and never a fresh admission (review B).
//! - Promotion selects the next eligible input UNDER the transaction that
//!   promotes it (review D): steers first in admission order, then at most
//!   ONE queued input at the caller-asserted idle boundary. The projector
//!   consumes the pending row only when its session matches the event's
//!   session (review C). Delivery vocabulary is closed (review E). Event ids
//!   come from an infallible mint, so a promotion boundary can never end
//!   part-complete for want of an id (review F).

use crate::event::{InboxEvent, OperationDeliverySettlement};
use grapt_store::{
    AggregateId, AppendInput, Appended, EventStore, GovernedTransaction, HeldWriters, ProjectionId,
    Projector, WriteError,
};
use grapt_types::{
    ArtifactDigest, ArtifactId, ArtifactRef, DeliveryClaimId, DeliveryMode, EventId, EventVersion,
    MessageId, OperationId, RuntimeGeneration, SessionId, SubagentOperationId,
};
use rusqlite::Transaction;
use serde_json::{json, Value};

mod acceptance;
mod compaction;
mod coordinates;
mod existing;
mod external_notice;
mod history;
pub mod images;
mod native_mail;
mod native_mail_state;
mod operation_delivery;
pub mod options;
pub use options::UserInputOptions;
mod pending;
mod schema;
mod synthetic;

pub use acceptance::InputAcceptance;
pub use compaction::{
    CompactionAdmission, CompactionAdmitRequest, CompactionFailRequest, CompactionFailure,
    PendingCompaction,
};
pub use native_mail_state::NativeMailResume;
pub use pending::PendingInput;
pub use synthetic::{PromotedInputKind, SyntheticAdmitRequest};

#[derive(Debug, thiserror::Error)]
pub enum InboxError {
    #[error(transparent)]
    Visibility(#[from] grapt_store::history_visibility::VisibilityError),
    #[error("compacted history: {0}")]
    Compaction(#[from] grapt_store::compaction::CompactionError),
    #[error("promoted history boundary {generation} is not a current committed cut for {session}")]
    HistoryBoundary {
        session: SessionId,
        generation: EventId,
    },
    #[error("session {session} has more than {limit} promoted user messages in the requested history window")]
    PromotedPageTooLong { session: SessionId, limit: usize },
    #[error("admission coordinate page exceeds {limit} inputs")]
    CoordinatePageTooLong { limit: usize },
    #[error("pending input catalog violates its bound: {0}")]
    PendingBound(&'static str),
    #[error("native inbox wake is held; explicit human resume is required")]
    NativeWakeHeld,
    #[error(transparent)]
    Store(#[from] WriteError),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("input {input} exists with a different {differs} (exact retry must match session, text, and delivery)")]
    Conflict {
        input: MessageId,
        differs: &'static str,
    },
    #[error("input {input}: durable record corrupt: {reason}")]
    Corruption { input: MessageId, reason: String },
    #[error("compaction {input} already ran; request a new one with a new id")]
    CompactionSettled { input: MessageId },
    #[error("pending row id {raw:?} corrupt: {reason}")]
    CorruptPendingId { raw: String, reason: String },
    #[error("event version 1 must exist")]
    Version,
    #[error(
        "operation delivery capacity exhausted for {session}: pending {pending}/{max_pending}, bytes {bytes}/{max_bytes}"
    )]
    DeliveryCapacity {
        session: SessionId,
        pending: u64,
        max_pending: u64,
        bytes: u64,
        max_bytes: u64,
    },
    #[error("no stored operation-delivery authority for subagent {subagent} claim {claim}")]
    MissingOperationDeliveryAuthority {
        subagent: SubagentOperationId,
        claim: DeliveryClaimId,
    },
    #[error("stored operation-delivery authority for subagent {subagent} is corrupt: {reason}")]
    CorruptOperationDeliveryAuthority {
        subagent: SubagentOperationId,
        reason: String,
    },
}

/// Typed wake disposition — advisory only; nothing in this slice schedules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeDisposition {
    /// `resume: false` — durable admission only.
    AdmitOnly,
    /// `resume: true` — the caller may advise its supervisor to wake.
    WakeAdvised,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AdmitOutcome {
    /// New durable admission at this event sequence.
    Admitted { seq: u64, wake: WakeDisposition },
    /// Exact retry of an existing admission (pending or already promoted).
    Reconciled { wake: WakeDisposition },
}

/// Presentation provenance is part of exact-retry identity, never authority.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UserInputOrigin {
    #[default]
    Human,
    SeatStartup,
}

#[derive(Debug)]
pub struct AdmitRequest<'a> {
    pub event_id: &'a EventId,
    pub session: &'a SessionId,
    pub input: &'a MessageId,
    pub text: &'a str,
    pub delivery: DeliveryMode,
    pub resume: bool,
    pub created_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromoteScope {
    /// Promote pending steers and native work explicitly resumed by a human.
    Steer,
    /// Steers first; then, because the caller asserts the Session is
    /// otherwise idle, exactly ONE ordinary queued input.
    InputAtIdle,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Promoted {
    pub input: MessageId,
    pub delivery: DeliveryMode,
    pub seq: u64,
}

fn delivery_str(d: DeliveryMode) -> &'static str {
    match d {
        DeliveryMode::Steer => "steer",
        DeliveryMode::Queue => "queue",
    }
}

fn delivery_from_str(raw: &str) -> Option<DeliveryMode> {
    match raw {
        "steer" => Some(DeliveryMode::Steer),
        "queue" => Some(DeliveryMode::Queue),
        _ => None,
    }
}

/// Projects an admitted event into `session_pending` and a promoted event
/// into pending-consumption + the visible `session_message` row — always
/// inside the append transaction (the bus.ts projector law). Public so a
/// Session fork projects the inputs it copies exactly as they were first
/// projected.
#[derive(Debug, Default)]
pub struct InboxProjector;

impl Projector for InboxProjector {
    fn projection(&self) -> ProjectionId {
        ProjectionId::new("inbox")
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn project(
        &mut self,
        tx: &Transaction<'_>,
        event: &grapt_store::DecodedEvent,
    ) -> Result<(), String> {
        let decoded =
            InboxEvent::decode(event.kind.name(), event.kind.version().value(), &event.data)
                .map_err(|e| e.to_string())?;
        match decoded {
            Some(InboxEvent::Admitted {
                session,
                input,
                delivery,
            }) => {
                // Text is REQUIRED (review C): an admitted input without text
                // is a contract violation, never an empty default.
                let text = event
                    .data
                    .pointer("/input/data/text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("{input}: admitted event has no input.data.text"))?;
                tx.execute(
                    "INSERT INTO session_pending
                         (id, session_id, delivery, text, admitted_seq, input_type, metadata_json)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'user', ?6)",
                    rusqlite::params![
                        input.as_str(),
                        session.as_str(),
                        delivery_str(delivery),
                        text,
                        event.seq.value() as i64,
                        event
                            .data
                            .pointer("/input/data/metadata")
                            .map(serde_json::to_string)
                            .transpose()
                            .map_err(|e| e.to_string())?,
                    ],
                )
                .map_err(|e| e.to_string())?;
            }
            Some(InboxEvent::SyntheticAdmitted {
                session,
                input,
                delivery,
            }) => {
                let text = event
                    .data
                    .pointer("/input/data/text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("{input}: synthetic event has no input.data.text"))?;
                let description = event
                    .data
                    .pointer("/input/data/description")
                    .and_then(Value::as_str);
                let metadata = event.data.pointer("/input/data/metadata");
                let metadata = metadata
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(|error| format!("{input}: synthetic metadata: {error}"))?;
                tx.execute(
                    "INSERT INTO session_pending
                         (id, session_id, delivery, text, admitted_seq, input_type,
                          description, metadata_json)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'synthetic', ?6, ?7)",
                    rusqlite::params![
                        input.as_str(),
                        session.as_str(),
                        delivery_str(delivery),
                        text,
                        event.seq.value() as i64,
                        description,
                        metadata,
                    ],
                )
                .map_err(|e| e.to_string())?;
            }
            Some(InboxEvent::Promoted { session, input }) => {
                // The pending row must belong to THIS session (review C): a
                // corrupt promoted event cannot move an input across
                // sessions.
                let (text, delivery, input_type, description, metadata): (
                    String,
                    String,
                    String,
                    Option<String>,
                    Option<String>,
                ) = tx
                    .query_row(
                        "SELECT text, delivery, input_type, description, metadata_json
                         FROM session_pending
                         WHERE id = ?1 AND session_id = ?2",
                        rusqlite::params![input.as_str(), session.as_str()],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                    )
                    .map_err(|e| format!("{input}: no pending row in session {session}: {e}"))?;
                if delivery_from_str(&delivery).is_none() {
                    return Err(format!(
                        "{input}: pending delivery {delivery:?} outside the closed vocabulary"
                    ));
                }
                if PromotedInputKind::from_str(&input_type).is_none() {
                    return Err(format!(
                        "{input}: pending input type {input_type:?} outside the closed vocabulary"
                    ));
                }
                if let Some(raw) = &metadata {
                    serde_json::from_str::<Value>(raw).map_err(|error| {
                        format!("{input}: pending metadata is corrupt: {error}")
                    })?;
                }
                tx.execute(
                    "DELETE FROM session_pending WHERE id = ?1 AND session_id = ?2",
                    rusqlite::params![input.as_str(), session.as_str()],
                )
                .map_err(|e| e.to_string())?;
                tx.execute(
                    "INSERT INTO session_message
                         (id, session_id, type, text, delivery, seq, description, metadata_json)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        input.as_str(),
                        session.as_str(),
                        input_type,
                        text,
                        delivery,
                        event.seq.value() as i64,
                        description,
                        metadata,
                    ],
                )
                .map_err(|e| e.to_string())?;
            }
            Some(InboxEvent::OperationDeliveryAdmitted {
                session,
                input,
                operation,
                subagent,
                claim,
                generation,
                settlement,
                artifact,
                projection,
            }) => {
                let projection_bytes = i64::try_from(projection.len())
                    .map_err(|_| format!("{input}: projection length does not fit storage"))?;
                let generation = i64::try_from(generation.value())
                    .map_err(|_| format!("{input}: generation does not fit storage"))?;
                let artifact_length = artifact
                    .as_ref()
                    .map(|value| {
                        i64::try_from(value.length())
                            .map_err(|_| format!("{input}: artifact length does not fit storage"))
                    })
                    .transpose()?;
                tx.execute(
                    "INSERT INTO session_operation_delivery_pending
                     (id, session_id, operation_id, subagent_operation_id, claim_id,
                      generation, settlement, artifact_id, artifact_length, artifact_digest,
                       projection, projection_bytes, admitted_seq)
                      VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    rusqlite::params![
                        input.as_str(),
                        session.as_str(),
                        operation.as_str(),
                        subagent.as_str(),
                        claim.as_str(),
                        generation,
                        settlement.as_str(),
                        artifact.as_ref().map(|value| value.id().as_str()),
                        artifact_length,
                        artifact.as_ref().map(|value| value.digest().as_str()),
                        projection,
                        projection_bytes,
                        i64::try_from(event.seq.value())
                            .map_err(|_| format!("{input}: sequence does not fit storage"))?,
                    ],
                )
                .map_err(|e| e.to_string())?;
            }
            Some(
                InboxEvent::LegacySyntheticAdmitted { .. } | InboxEvent::AgentMessageAdmitted(_),
            ) => {}
            Some(InboxEvent::CompactionAdmitted { session, input }) => {
                if compaction::pending_in(tx, &session)
                    .map_err(|e| e.to_string())?
                    .is_some()
                {
                    return Err(format!(
                        "{input}: a compaction is already pending in session {session}"
                    ));
                }
                tx.execute(
                    "INSERT INTO session_pending
                         (id, session_id, delivery, text, admitted_seq, input_type)
                     VALUES (?1, ?2, 'queue', '', ?3, 'compaction')",
                    rusqlite::params![
                        input.as_str(),
                        session.as_str(),
                        i64::try_from(event.seq.value())
                            .map_err(|_| format!("{input}: sequence does not fit storage"))?,
                    ],
                )
                .map_err(|e| e.to_string())?;
            }
            Some(InboxEvent::CompactionSettled { session }) => {
                tx.execute(
                    "DELETE FROM session_pending
                     WHERE session_id = ?1 AND input_type = 'compaction'",
                    [session.as_str()],
                )
                .map_err(|e| e.to_string())?;
            }
            None => {}
        }
        Ok(())
    }
}

/// The durable Session inbox. Owns the synchronous store; every mutation —
/// including its decision reads — is one immediate transaction.
#[derive(Debug)]
pub struct Inbox {
    store: StoreHandle,
}

#[derive(Debug)]
enum StoreHandle {
    Writer(EventStore),
    Reader(grapt_store::read_pool::ReadLease),
}

impl std::ops::Deref for StoreHandle {
    type Target = EventStore;
    fn deref(&self) -> &EventStore {
        match self {
            Self::Writer(store) => store,
            Self::Reader(read) => read.store(),
        }
    }
}

impl std::ops::DerefMut for StoreHandle {
    fn deref_mut(&mut self) -> &mut EventStore {
        match self {
            Self::Writer(store) => store,
            Self::Reader(read) => read.store_mut(),
        }
    }
}

/// Exact durable coordinates of one admitted input, exposed as a typed Inbox
/// projection so product code never reaches into the event table or decodes
/// provider-shaped JSON on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionCoordinates {
    pub seq: u64,
    pub created_ms: i64,
}

/// One promoted input as the Inbox projector committed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotedInput {
    pub text: String,
    pub seq: u64,
    pub metadata: Option<Value>,
}

/// One projected user message with the identity and durable order needed to
/// rebuild a public transcript after a process restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotedMessage {
    pub input: MessageId,
    pub kind: PromotedInputKind,
    pub text: String,
    pub description: Option<String>,
    pub metadata: Option<Value>,
    pub seq: u64,
}

impl Inbox {
    pub fn new(store: EventStore) -> Result<Self, InboxError> {
        Self::ensure_tables(store.connection())?;
        Ok(Self {
            store: StoreHandle::Writer(store),
        })
    }

    /// Borrow an already initialized database without DDL or writer authority.
    pub fn from_read(read: grapt_store::read_pool::ReadLease) -> Self {
        Self {
            store: StoreHandle::Reader(read),
        }
    }

    /// End a finite read projection and return its idle connection to the pool.
    pub fn finish_read(self) -> Result<(), grapt_store::read_pool::ReadPoolError> {
        match self.store {
            StoreHandle::Reader(read) => read.finish(),
            StoreHandle::Writer(_) => Ok(()),
        }
    }

    /// End a finite read projection whose connection must not be reused.
    ///
    /// A read that failed mid-projection may have left its reader unable to
    /// reach the database it just failed on, so this closes it and returns only
    /// the capacity. See [`ReadLease::discard`](grapt_store::read_pool::ReadLease::discard).
    pub fn discard_read(self) {
        match self.store {
            StoreHandle::Reader(read) => read.discard(),
            StoreHandle::Writer(_) => {}
        }
    }

    pub fn store(&self) -> &EventStore {
        &self.store
    }

    /// Mutable store access for the OTHER durable writers that share this
    /// Session's aggregate — the execution owner's Step facts, for instance.
    ///
    /// Admission and promotion stay on the writer connection. A pooled inbox
    /// has a database-enforced read-only connection and cannot commit.
    pub fn store_mut(&mut self) -> &mut EventStore {
        &mut self.store
    }

    /// Durable admission: reconciliation reads, the decision, and the
    /// append+projection all inside ONE immediate transaction (review A).
    pub fn admit(&mut self, request: AdmitRequest<'_>) -> Result<AdmitOutcome, InboxError> {
        self.admit_with_images(request, &[])
    }

    /// Capture pixels first; append these exact immutable references atomically with admission.
    pub fn admit_with_images(
        &mut self,
        request: AdmitRequest<'_>,
        images: &[grapt_types::ArtifactRef],
    ) -> Result<AdmitOutcome, InboxError> {
        self.admit_with_origin(request, images, UserInputOrigin::Human)
    }

    /// The existing user admission transaction also owns startup provenance.
    pub fn admit_with_origin(
        &mut self,
        request: AdmitRequest<'_>,
        images: &[grapt_types::ArtifactRef],
        origin: UserInputOrigin,
    ) -> Result<AdmitOutcome, InboxError> {
        self.admit_with_options(
            request,
            UserInputOptions {
                images,
                origin,
                max_steps: None,
            },
        )
    }

    /// Execution bounds and images are part of the exact admission identity.
    pub fn admit_with_options(
        &mut self,
        request: AdmitRequest<'_>,
        options: UserInputOptions<'_>,
    ) -> Result<AdmitOutcome, InboxError> {
        let metadata = options.metadata()?;
        let wake = if request.resume {
            WakeDisposition::WakeAdvised
        } else {
            WakeDisposition::AdmitOnly
        };
        let held = self.store.held_writers();
        let aggregate = AggregateId::Session(request.session.clone());
        let tx = self.store.governed_transaction()?;

        if let Some(existing) = Self::find_existing(&tx, request.input)? {
            let outcome = Self::reconcile_user(&request, existing, wake, metadata.as_ref());
            tx.commit()?.discard();
            return outcome;
        }

        let mut data = json!({
            "sessionID": request.session.as_str(),
            "inputID": request.input.as_str(),
            "input": {
                "type": "user",
                "data": { "text": request.text },
                "delivery": delivery_str(request.delivery),
            },
        });
        if let Some(metadata) = metadata {
            data["input"]["data"]["metadata"] = metadata;
        }
        let version = EventVersion::new(1).ok_or(InboxError::Version)?;
        let appended = EventStore::append_within(
            &tx,
            AppendInput {
                id: request.event_id,
                aggregate: aggregate.clone(),
                writer: held.token(&aggregate)?,
                kind_name: "session.input.admitted",
                version,
                created_ms: request.created_ms,
                data: &data,
            },
            &mut [&mut InboxProjector],
        )?;
        tx.commit()?.discard();
        let seq = match appended {
            Appended::Committed { seq } | Appended::Idempotent { seq } => seq,
        };
        Ok(AdmitOutcome::Admitted { seq, wake })
    }

    /// The durable sequence at which a promoted input entered projected
    /// history, or `None` if this input has not been promoted into this
    /// Session.
    ///
    /// This is the read that lets an execution adapter prove a promotion is
    /// durable before it builds a Step on top of it: the projected user
    /// message and the promoted event commit in one transaction, so a row
    /// here means the transcript boundary has actually happened.
    pub fn promoted_cursor(
        &self,
        session: &SessionId,
        input: &MessageId,
    ) -> Result<Option<u64>, InboxError> {
        let seq: Option<i64> = self
            .store
            .connection()
            .query_row(
                "SELECT seq FROM session_message
                 WHERE id = ?1 AND session_id = ?2 AND type IN ('user', 'synthetic')",
                rusqlite::params![input.as_str(), session.as_str()],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| {
                if e == rusqlite::Error::QueryReturnedNoRows {
                    Ok(None)
                } else {
                    Err(e)
                }
            })?;
        match seq {
            None => Ok(None),
            Some(raw) => u64::try_from(raw).map(Some).map_err(|_| {
                // A negative sequence is corruption, not a position. Returning
                // it as 0 would place a Step before history it must follow.
                InboxError::Corruption {
                    input: input.clone(),
                    reason: format!("projected message sequence {raw} is negative"),
                }
            }),
        }
    }

    /// Read the exact projected user message produced by promotion.
    pub fn promoted_input(
        &self,
        session: &SessionId,
        input: &MessageId,
    ) -> Result<PromotedInput, InboxError> {
        let (text, seq, metadata_bounded, metadata): (String, i64, bool, Option<String>) =
            self.store.connection().query_row(
                "SELECT text, seq,
                    metadata_json IS NULL OR length(CAST(metadata_json AS BLOB)) <= 65536,
                    CASE WHEN length(CAST(metadata_json AS BLOB)) <= 65536 THEN metadata_json END
             FROM session_message
             WHERE id = ?1 AND session_id = ?2 AND type IN ('user', 'synthetic')",
                rusqlite::params![input.as_str(), session.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        if !metadata_bounded {
            return Err(InboxError::PendingBound(
                "promoted metadata exceeds 65536 bytes",
            ));
        }
        let metadata = metadata
            .map(|raw| serde_json::from_str(&raw))
            .transpose()
            .map_err(|_| InboxError::Corruption {
                input: input.clone(),
                reason: "promoted metadata is invalid JSON".into(),
            })?;
        let seq = u64::try_from(seq).map_err(|_| InboxError::Corruption {
            input: input.clone(),
            reason: format!("projected message sequence {seq} is negative"),
        })?;
        if !grapt_store::history_visibility::HistoryVisibility::read(
            self.store.connection(),
            session,
        )?
        .contains(seq)
        {
            return Err(InboxError::Corruption {
                input: input.clone(),
                reason: "requested input was removed from the conversation".into(),
            });
        }
        Ok(PromotedInput {
            text,
            seq,
            metadata,
        })
    }

    /// Promote every eligible pending input at this boundary. Each iteration
    /// selects the NEXT eligible input under the same immediate transaction
    /// that appends its promoted event and consumes it (review D) —
    /// concurrent promoters serialize into ordering, never a stale plan.
    /// `mint` is infallible, so a boundary can never end part-complete for
    /// want of an id (review F).
    pub fn promote(
        &mut self,
        session: &SessionId,
        scope: PromoteScope,
        mint: &mut dyn FnMut() -> EventId,
        created_ms: i64,
    ) -> Result<Vec<Promoted>, InboxError> {
        let mut promoted = Vec::new();
        let mut queue_taken = false;
        loop {
            // Once a queued input has been taken, the remaining iterations are
            // steer-only: `InputAtIdle` permits one ordinary queued input per
            // boundary.
            let scope = if queue_taken {
                PromoteScope::Steer
            } else {
                scope
            };
            match self.promote_next(session, scope, mint, created_ms)? {
                Some(next) => {
                    queue_taken |= next.delivery == DeliveryMode::Queue;
                    promoted.push(next);
                }
                None => break,
            }
        }
        Ok(promoted)
    }

    /// Promote AT MOST ONE pending input, in one immediate transaction.
    ///
    /// This is the unit an execution drain needs. One input, one transaction:
    /// the caller decides how many of these make a boundary, and a queued
    /// input's promotion is separated from the Step that answers it by a real
    /// decision rather than by a loop it cannot interrupt.
    ///
    /// A drain calls this repeatedly at one boundary to promote a batch of
    /// steers — which is correct, because the user typed all of them into the
    /// same turn — but never to sweep a queue: exactly one queued input is
    /// offered per idle boundary, and continuation is reevaluated before
    /// another.
    ///
    /// The order is still the law: the earliest steer wins, and a queued input
    /// is offered only under [`PromoteScope::InputAtIdle`], when no steer is
    /// eligible. A genuine human promotion also resumes previously stopped
    /// native notices before its next Step; it does not resume other queues.
    pub fn promote_next(
        &mut self,
        session: &SessionId,
        scope: PromoteScope,
        mint: &mut dyn FnMut() -> EventId,
        created_ms: i64,
    ) -> Result<Option<Promoted>, InboxError> {
        let held = self.store.held_writers();
        let aggregate = AggregateId::Session(session.clone());
        let tx = self.store.governed_transaction()?;

        // A pending compaction never holds input: it runs inside the next
        // promoted input's first Step, so holding that input would leave it
        // pending forever.
        let promoted = {
            // A closed delivery vocabulary must reject corrupt durable work,
            // not hide it behind the valid-value filters below. Otherwise an
            // unknown row can strand a Session while this boundary reports no
            // eligible work.
            let invalid: Option<(String, String)> = tx
                .query_row(
                    "SELECT id, delivery FROM session_pending
                     WHERE session_id = ?1 AND delivery NOT IN ('steer', 'queue')
                     ORDER BY admitted_seq LIMIT 1",
                    [session.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map(Some)
                .or_else(|e| {
                    if e == rusqlite::Error::QueryReturnedNoRows {
                        Ok(None)
                    } else {
                        Err(e)
                    }
                })?;
            if let Some((input_raw, delivery_raw)) = invalid {
                let input: MessageId = input_raw.parse().map_err(|e: grapt_types::IdError| {
                    InboxError::CorruptPendingId {
                        raw: input_raw,
                        reason: e.to_string(),
                    }
                })?;
                return Err(InboxError::Corruption {
                    input,
                    reason: format!(
                        "pending delivery {delivery_raw:?} outside the closed vocabulary"
                    ),
                });
            }

            // Use the bounded, corroborated pending fold inside the promotion
            // transaction. A held or superseded native notice must not block a
            // later human input, and its authority must still be current here.
            let mut pending = Self::pending_inputs_for_promotion_within(&tx, session)?;
            pending.sort_by_key(|input| (input.delivery != DeliveryMode::Steer, input.seq));
            let mut next = None;
            for candidate in pending {
                if candidate.delivery == DeliveryMode::Queue
                    && scope != PromoteScope::InputAtIdle
                    && !Self::resumed_native_queue_within(&tx, session, &candidate)?
                {
                    continue;
                }
                if Self::native_mail_input_eligible_within(&tx, session, &candidate)? {
                    next = Some(candidate);
                    break;
                }
            }
            let Some(selected) = next else {
                tx.commit()?.discard();
                return Ok(None);
            };
            Self::resume_native_mail_after_human_within(
                &tx, &held, session, &selected, created_ms,
            )?;
            let input = selected.input;
            let delivery = selected.delivery;

            let event_id = mint();
            let data = json!({
                "sessionID": session.as_str(),
                "inputID": input.as_str(),
            });
            let version = EventVersion::new(1).ok_or(InboxError::Version)?;
            let appended = EventStore::append_within(
                &tx,
                AppendInput {
                    id: &event_id,
                    aggregate: aggregate.clone(),
                    writer: held.token(&aggregate)?,
                    kind_name: "session.input.promoted",
                    version,
                    created_ms,
                    data: &data,
                },
                &mut [&mut InboxProjector],
            )?;
            tx.commit()?.discard();
            let seq = match appended {
                Appended::Committed { seq } | Appended::Idempotent { seq } => seq,
            };
            Promoted {
                input,
                delivery,
                seq,
            }
        };
        Ok(Some(promoted))
    }
}
