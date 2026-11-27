//! What a restart settles before the server admits any work.
//!
//! The inbox and executor do not exist yet, so nothing can be running. The
//! attempt receipts, forms and statuses process loss left open are settled
//! here, and so are the nested tool runs that died with the process, for every
//! Session the crash interrupted: one recorded as running or waiting, or one
//! with an open attempt receipt. Each lost run gains exactly the unknown-effect
//! terminal `/reconcile` would record. A run left open on a Session that was
//! already idle is not process loss this pass can prove, so it keeps
//! `/reconcile`, and startup never scans every Session's history.
//!
//! Every step reports what it skipped as a warning and lets startup continue.
//! A Session's settlement commits on its own; nothing after it can undo or
//! misreport it.
use super::*;
use std::collections::BTreeSet;

pub(super) fn recover(
    store: &mut grapt_store::EventStore,
    ids: &Arc<OsRandomIds>,
    clock: &Arc<SystemClock>,
    receipt_bytes: NonZeroU32,
) -> Result<(), DogfoodRefusal> {
    let attempts = recover_process_loss_attempts(
        store,
        clock.as_ref(),
        AttemptRecordCeiling::new(receipt_bytes),
    )
    .map_err(|error| DogfoodRefusal::Compose(error.to_string()))?;
    for warning in &attempts.warnings {
        crate::server_log::warn(format_args!(
            "startup warning reason=attempt-recovery-skipped detail={warning}"
        ));
    }
    omitted("attempt-recovery-skipped", attempts.warnings_omitted);
    let forms = elicitation::recover(store, ids.as_ref(), clock.as_ref())
        .map_err(DogfoodRefusal::Compose)?;
    for warning in &forms {
        crate::server_log::warn(format_args!(
            "startup warning reason=form-recovery-skipped detail={warning}"
        ));
    }
    let statuses = session_status::SessionStatusEmitter::recover(store, clock, ids);
    let interrupted: BTreeSet<SessionId> = statuses.into_iter().chain(attempts.touched).collect();
    settle_lost_tool_runs(store, &interrupted, ids, clock);
    Ok(())
}

fn omitted(reason: &str, count: usize) {
    if count > 0 {
        crate::server_log::warn(format_args!(
            "startup warning reason={reason} detail={count} more not listed"
        ));
    }
}

/// Record each interrupted Session's lost tool runs as unknown, one Session
/// per transaction. A Session whose runs cannot be read or settled, including
/// one with more than reconciliation may settle, is skipped with a warning and
/// keeps its `/reconcile` refusal.
fn settle_lost_tool_runs(
    store: &mut grapt_store::EventStore,
    sessions: &BTreeSet<SessionId>,
    ids: &Arc<OsRandomIds>,
    clock: &Arc<SystemClock>,
) {
    let ids = execution::RuntimeEventIds(Arc::clone(ids));
    let clock = execution::RuntimeClock(Arc::clone(clock));
    for session in sessions {
        let settled = continuation::orphaned_nested(store, session).and_then(|orphans| {
            if orphans.is_empty() {
                return Ok(0);
            }
            elected(store, session)?;
            grapt_runtime::reconcile_orphaned_nested(store, session, &orphans, &ids, &clock)?;
            Ok(orphans.len())
        });
        match settled {
            Ok(0) => {}
            Ok(count) => crate::server_log::info(format_args!(
                "session {session}: recorded {count} tool runs lost at restart as unknown"
            )),
            Err(reason) => crate::server_log::warn(format_args!(
                "startup warning reason=lost-tool-runs-skipped detail=session {session}: {reason}; \
                 /reconcile records them"
            )),
        }
    }
}

fn elected(store: &mut grapt_store::EventStore, session: &SessionId) -> Result<(), String> {
    let aggregate = grapt_store::AggregateId::Session(session.clone());
    match store.fencing_token(&aggregate) {
        Ok(_) => Ok(()),
        Err(grapt_store::WriteError::NotElected { .. }) => store
            .elect_writer(&aggregate)
            .map(drop)
            .map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    }
}
