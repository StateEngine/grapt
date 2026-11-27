//! Settles nested starts that process loss left without a terminal.
//!
//! A nested call running when its process died has a durable start and
//! nothing else, so whether its effect happened is unknown and the Session
//! refuses to continue past it. This owner records exactly that, at startup
//! for every Session the crash interrupted and otherwise only when asked
//! (`/reconcile`) for an idle Session: one unknown-effect terminal per
//! orphaned `(attempt, call)`, never success, never a retry, and never in place
//! of a terminal that already exists. An unreadable terminal row is not one:
//! it proves nothing, so its call is settled beside it and the row stays. The
//! live attempt owner cannot do this: reopening a past attempt would bind it
//! to a second assistant message.

use grapt_event::session_tool_terminal::ToolTerminalFact;
use grapt_event::AggregateId;
use grapt_store::write::EventStore;
use grapt_types::{
    PhysicalAttemptId, SessionId, ToolCallId, ToolFailureReason, ToolTerminalOutcome,
};

use crate::application::{Clock, EventIds};

/// Three active compositions of at most 64 nested effects each: a larger
/// unresolved set is not process loss this owner may settle.
pub const MAX_ORPHANED_NESTED: usize = 3 * 64;

/// Record an unknown effect for each orphaned nested start, in one
/// transaction. The caller holds the Session idle and elected, and read
/// `orphans` under the same store lock, so none has gained a terminal since.
pub fn reconcile_orphaned_nested(
    store: &mut EventStore,
    session: &SessionId,
    orphans: &[(PhysicalAttemptId, ToolCallId)],
    ids: &dyn EventIds,
    clock: &dyn Clock,
) -> Result<(), String> {
    if orphans.len() > MAX_ORPHANED_NESTED {
        return Err("unresolved nested effects exceed what reconciliation may settle".into());
    }
    if orphans.is_empty() {
        return Ok(());
    }
    let writer = store
        .fencing_token(&AggregateId::Session(session.clone()))
        .map_err(|error| format!("reconciliation has no Session writer: {error}"))?;
    let now = clock.now_ms();
    let tx = store
        .governed_transaction()
        .map_err(|error| format!("reconciliation transaction refused: {error}"))?;
    for (attempt, call) in orphans {
        let fact = ToolTerminalFact::new(
            attempt.clone(),
            call.clone(),
            ToolTerminalOutcome::Failed {
                reason: ToolFailureReason::EffectUnknown,
                detail: None,
            },
        );
        grapt_store::tool_terminal::record(&tx, session, &ids.mint(), writer, now, &fact, &mut [])
            .map_err(|error| format!("reconciliation terminal refused: {error}"))?;
    }
    tx.commit()
        .map_err(|error| format!("reconciliation commit refused: {error}"))?
        .discard();
    Ok(())
}
