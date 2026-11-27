//! Governed appends for the assistant message a Step produces.
//!
//! Typed seams, one per Rust-native fact in
//! [`grapt_event::session_message`]. Each owns only the synchronous event
//! transaction: the caller owns commit and publication, so the event, every
//! projector row and every projection watermark commit together and the
//! advisory notice cannot leave the transaction before commit.
//!
//! Identity, time and the fenced writer are accepted from the caller and
//! forwarded unchanged — these seams never mint an identity and never read a
//! clock, exactly as [`crate::tool_terminal`] and [`crate::step_ended`] do.
//! That is what makes an exact retry reconcile as
//! [`Appended::Idempotent`] rather than append a second copy: the runtime
//! owner stamps one stable `EventId` per fact BEFORE the call, so a retried
//! commit carries the same identity and the same bytes.
//!
//! # Why the functions are separate
//!
//! A single `record(fact: SessionMessageFact)` would need a sum type over the
//! payloads, and every caller would then be able to name any of them
//! wherever one was expected. The Step owner writes the OPENED fact exactly
//! once and content items many times; making that difference visible in the
//! function it calls is the cheapest place for it to be visible at all.
//!
//! # Artifact capture is not this seam's business
//!
//! A content item references its payload. Capture is a separate recovery unit
//! and must already have committed before the caller constructs the fact — the
//! same precondition [`crate::tool_terminal`] states. These functions verify
//! nothing about the artifact store; they cannot, because they hold only the
//! event transaction.

use crate::governed::GovernedTransaction;
use crate::write::{AppendInput, Appended, FencedWriter, Projector, WriteError};
use grapt_event::session_message::checkpoint::{self, ContentCheckpointedFact};
use grapt_event::session_message::{
    self, AssistantMessageOpenedFact, ContentAppendedFact, ContentBlockOpenedFact,
    UsageObservedFact,
};
use grapt_event::AggregateId;
use grapt_types::{EventId, EventVersion, SessionId};

const OPENED: &str = "session.assistant-message.opened";
const BLOCK_OPENED: &str = "session.content-block.opened";
const APPENDED: &str = "session.content.appended";
const CHECKPOINTED: &str = "session.content.checkpointed";
const OBSERVED: &str = "session.usage.observed";

/// Appends the assistant message identity for one logical Step.
pub fn record_opened(
    tx: &GovernedTransaction<'_>,
    session: &SessionId,
    id: &EventId,
    writer: FencedWriter,
    created_ms: i64,
    fact: &AssistantMessageOpenedFact,
    projectors: &mut [&mut dyn Projector],
) -> Result<Appended, WriteError> {
    append(
        tx,
        session,
        id,
        writer,
        created_ms,
        OPENED,
        if fact.selection().is_some() { 2 } else { 1 },
        &session_message::encode_opened(fact),
        projectors,
    )
}

/// Appends a streaming block's identity before transient bytes are offered.
pub fn record_block_opened(
    tx: &GovernedTransaction<'_>,
    session: &SessionId,
    id: &EventId,
    writer: FencedWriter,
    created_ms: i64,
    fact: &ContentBlockOpenedFact,
    projectors: &mut [&mut dyn Projector],
) -> Result<Appended, WriteError> {
    append(
        tx,
        session,
        id,
        writer,
        created_ms,
        BLOCK_OPENED,
        1,
        &session_message::encode_block_opened(fact),
        projectors,
    )
}

/// Appends one content item at its exact ordinal.
pub fn record_appended(
    tx: &GovernedTransaction<'_>,
    session: &SessionId,
    id: &EventId,
    writer: FencedWriter,
    created_ms: i64,
    fact: &ContentAppendedFact,
    projectors: &mut [&mut dyn Projector],
) -> Result<Appended, WriteError> {
    append(
        tx,
        session,
        id,
        writer,
        created_ms,
        APPENDED,
        1,
        &session_message::encode_appended(fact),
        projectors,
    )
}

/// Appends an open text block's text so far (CLAIM-2). Never a content item:
/// only process-loss recovery promotes the newest one.
pub fn record_checkpointed(
    tx: &GovernedTransaction<'_>,
    session: &SessionId,
    id: &EventId,
    writer: FencedWriter,
    created_ms: i64,
    fact: &ContentCheckpointedFact,
    projectors: &mut [&mut dyn Projector],
) -> Result<Appended, WriteError> {
    append(
        tx,
        session,
        id,
        writer,
        created_ms,
        CHECKPOINTED,
        1,
        &checkpoint::encode_checkpointed(fact),
        projectors,
    )
}

/// Appends one usage observation for one Physical Attempt.
pub fn record_observed(
    tx: &GovernedTransaction<'_>,
    session: &SessionId,
    id: &EventId,
    writer: FencedWriter,
    created_ms: i64,
    fact: &UsageObservedFact,
    projectors: &mut [&mut dyn Projector],
) -> Result<Appended, WriteError> {
    append(
        tx,
        session,
        id,
        writer,
        created_ms,
        OBSERVED,
        2,
        &session_message::encode_observed(fact),
        projectors,
    )
}

/// The one place these kinds reach the append chokepoint.
///
/// Private, and its kind name and version come only from the public
/// record methods above, so there is no route through this module to append an
/// arbitrary pair. Message opening, content and checkpoints are v1; new usage observations
/// are v2 while the already-shipped usage v1 remains read-only.
#[allow(clippy::too_many_arguments)]
fn append(
    tx: &GovernedTransaction<'_>,
    session: &SessionId,
    id: &EventId,
    writer: FencedWriter,
    created_ms: i64,
    kind_name: &'static str,
    kind_version: u32,
    payload: &serde_json::Value,
    projectors: &mut [&mut dyn Projector],
) -> Result<Appended, WriteError> {
    let version = match EventVersion::new(kind_version) {
        Some(version) => version,
        None => unreachable!("owned session-message versions are non-zero"),
    };
    crate::EventStore::append_within(
        tx,
        AppendInput {
            id,
            aggregate: AggregateId::Session(session.clone()),
            writer,
            kind_name,
            version,
            created_ms,
            data: payload,
        },
        projectors,
    )
}
