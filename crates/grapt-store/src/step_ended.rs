//! Durable storage for the Rust-native `session.step.ended.2` fact.
//!
//! This module owns only the synchronous store seam. The strict payload is
//! owned by [`grapt_event::session_step_ended`], and the runtime owner remains
//! responsible for capturing one stable [`EventId`] and `created_ms` per
//! logical Step before terminal settlement. Both coordinates are accepted from
//! the caller and forwarded unchanged: this seam never mints an identity and
//! never reads a clock.
//!
//! It is deliberately the mirror of [`crate::step_failed`] and adds no second
//! route to a terminal. A Step has exactly one durable terminal fact; which of
//! the two it is belongs to the caller's classification, not to this seam.
//!
//! The session exists only as the event aggregate. It is deliberately absent
//! from the payload, so there is no second session coordinate that can drift.
//! Same-owner exact retries retain [`Appended::Idempotent`]; a changed payload,
//! timestamp, session, or writer epoch retains the governed append path's
//! [`crate::WriteError::Divergence`] refusal.

use crate::governed::GovernedTransaction;
use crate::write::{AppendInput, Appended, FencedWriter, Projector, WriteError};
use grapt_event::session_step_ended::{self, StepEndedFact};
use grapt_event::AggregateId;
use grapt_types::{EventId, EventVersion, SessionId};

/// The durable name of the Rust-native Step-success fact.
///
/// Version remains a separate coordinate because [`AppendInput`] composes the
/// stored `session.step.ended.2` identity from the accepted name and version.
/// The name is shared with the frozen oracle's v1; only the pair identifies the
/// contract, which is why the version is never folded into this constant.
const STEP_ENDED: &str = "session.step.ended";

/// Appends one `session.step.ended.2` fact inside the caller's transaction.
///
/// The caller owns commit and publication. Consequently the event, every
/// projector row, and every projection watermark commit together, while the
/// advisory notice cannot leave the transaction before commit.
pub fn record(
    tx: &GovernedTransaction<'_>,
    session: &SessionId,
    id: &EventId,
    writer: FencedWriter,
    created_ms: i64,
    fact: &StepEndedFact,
    projectors: &mut [&mut dyn Projector],
) -> Result<Appended, WriteError> {
    let aggregate = AggregateId::Session(session.clone());
    let payload = session_step_ended::encode(fact);
    let version = match EventVersion::new(2) {
        Some(version) => version,
        None => unreachable!("2 is a non-zero version"),
    };

    crate::EventStore::append_within(
        tx,
        AppendInput {
            id,
            aggregate,
            writer,
            kind_name: STEP_ENDED,
            version,
            created_ms,
            data: &payload,
        },
        projectors,
    )
}

/// Append a v3 terminal with an independently observed completion instant.
pub fn record_observed(
    tx: &GovernedTransaction<'_>,
    session: &SessionId,
    id: &EventId,
    writer: FencedWriter,
    created_ms: i64,
    fact: &grapt_event::session_step_observed::ObservedStep<StepEndedFact>,
    projectors: &mut [&mut dyn Projector],
) -> Result<Appended, WriteError> {
    let aggregate = AggregateId::Session(session.clone());
    let payload = grapt_event::session_step_observed::encode_ended(fact);
    let version = match EventVersion::new(3) {
        Some(version) => version,
        None => unreachable!("3 is a non-zero version"),
    };

    crate::EventStore::append_within(
        tx,
        AppendInput {
            id,
            aggregate,
            writer,
            kind_name: STEP_ENDED,
            version,
            created_ms,
            data: &payload,
        },
        projectors,
    )
}
