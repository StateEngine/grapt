//! Durable storage for the Rust-native `session.step.failed.2` fact.
//!
//! This module owns only the synchronous store seam. The strict payload is
//! owned by [`grapt_event::session_step_failed`], and the future runtime owner
//! remains responsible for capturing one stable [`EventId`] and `created_ms`
//! per logical Step before terminal settlement. Both coordinates are accepted
//! from the caller and forwarded unchanged: this seam never mints an identity
//! and never reads a clock.
//!
//! The session exists only as the event aggregate. It is deliberately absent
//! from the payload, so there is no second session coordinate that can drift.
//! Same-owner exact retries retain [`Appended::Idempotent`]; a changed payload,
//! timestamp, session, or writer epoch retains the governed append path's
//! [`crate::WriteError::Divergence`] refusal.

use crate::governed::GovernedTransaction;
use crate::write::{AppendInput, Appended, FencedWriter, Projector, WriteError};
use grapt_event::session_step_failed::{self, StepFailedFact};
use grapt_event::AggregateId;
use grapt_types::{EventId, EventVersion, SessionId};

/// The durable name of the Rust-native Step-failure fact.
///
/// Version remains a separate coordinate because [`AppendInput`] composes the
/// stored `session.step.failed.2` identity from the accepted name and version.
const STEP_FAILED: &str = "session.step.failed";

/// Appends one `session.step.failed.2` fact inside the caller's transaction.
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
    fact: &StepFailedFact,
    projectors: &mut [&mut dyn Projector],
) -> Result<Appended, WriteError> {
    let aggregate = AggregateId::Session(session.clone());
    let payload = session_step_failed::encode(fact)?;
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
            kind_name: STEP_FAILED,
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
    fact: &grapt_event::session_step_observed::ObservedStep<StepFailedFact>,
    projectors: &mut [&mut dyn Projector],
) -> Result<Appended, WriteError> {
    let aggregate = AggregateId::Session(session.clone());
    let payload = grapt_event::session_step_observed::encode_failed(fact)?;
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
            kind_name: STEP_FAILED,
            version,
            created_ms,
            data: &payload,
        },
        projectors,
    )
}
