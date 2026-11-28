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
//! An open receipt or an active status is the only durable mark that a crash
//! interrupted its Session, so a Session's lost runs are settled before either
//! mark is cleared. If they cannot be, the marks stay and the next start finds
//! the Session again. Sessions are visited page by page as each pass reads
//! them, so nothing here grows with the number of interrupted Sessions.
//!
//! Every step reports what it skipped as a warning and lets startup continue.
//! A Session's settlement commits on its own; nothing after it can undo or
//! misreport it.
use super::*;

pub(in crate::dogfood) mod auto_resume;

pub(super) fn recover(
    store: &mut grapt_store::EventStore,
    ids: &Arc<OsRandomIds>,
    clock: &Arc<SystemClock>,
    receipt_bytes: NonZeroU32,
) -> Result<(), DogfoodRefusal> {
    auto_resume::ensure_schema_if_missing(store).map_err(DogfoodRefusal::Compose)?;
    let runtime_ids = execution::RuntimeEventIds(Arc::clone(ids));
    let runtime_clock = execution::RuntimeClock(Arc::clone(clock));
    let mut attempt_first = |store: &mut grapt_store::EventStore, session: &SessionId| {
        settle_lost_tool_runs(store, session, &runtime_ids, &runtime_clock)
    };
    let attempts = recover_process_loss_attempts(
        store,
        clock.as_ref(),
        AttemptRecordCeiling::new(receipt_bytes),
        &mut attempt_first,
    )
    .map_err(|error| DogfoodRefusal::Compose(error.to_string()))?;
    for warning in &attempts.warnings {
        crate::server_log::warn(format_args!(
            "startup warning reason=attempt-recovery-skipped detail={warning}"
        ));
    }
    if attempts.warnings_omitted > 0 {
        crate::server_log::warn(format_args!(
            "startup warning reason=attempt-recovery-skipped detail={} more not listed",
            attempts.warnings_omitted
        ));
    }
    let pre_recovery_forms = auto_resume::collect_pre_recovery_form_sessions(store.connection())
        .map_err(DogfoodRefusal::Compose)?;
    let forms = elicitation::recover(store, ids.as_ref(), clock.as_ref())
        .map_err(DogfoodRefusal::Compose)?;
    for warning in &forms {
        crate::server_log::warn(format_args!(
            "startup warning reason=form-recovery-skipped detail={warning}"
        ));
    }
    let mut status_first = |store: &mut grapt_store::EventStore, session: &SessionId| {
        settle_lost_tool_runs(store, session, &runtime_ids, &runtime_clock)?;
        auto_resume::enqueue_if_eligible(
            store,
            session,
            &pre_recovery_forms,
            clock.as_ref(),
            ids.as_ref(),
        )
    };
    session_status::SessionStatusEmitter::recover(store, clock, ids, &mut status_first);
    Ok(())
}

/// Record one interrupted Session's lost tool runs as unknown, in one
/// transaction; the caller holds its writer. An error keeps the Session's
/// crash marks for the next start. A history `/reconcile` refuses too, being
/// corrupt or larger than reconciliation may settle, is reported and does not
/// hold the Session's marks back, since no later start could settle it either.
fn settle_lost_tool_runs(
    store: &mut grapt_store::EventStore,
    session: &SessionId,
    ids: &execution::RuntimeEventIds,
    clock: &execution::RuntimeClock,
) -> Result<(), String> {
    let orphans = match continuation::orphaned_nested(store, session) {
        Ok(orphans) => orphans,
        Err(continuation::OrphanScan::Unsettleable(reason)) => {
            crate::server_log::warn(format_args!(
                "startup warning reason=lost-tool-runs-unsettleable detail=session {session}: \
                 {reason}; neither restart nor /reconcile can record them"
            ));
            return Ok(());
        }
        Err(continuation::OrphanScan::Unreadable(reason)) => {
            return Err(format!("its lost tool runs could not be read: {reason}"))
        }
    };
    if orphans.is_empty() {
        return Ok(());
    }
    grapt_runtime::reconcile_orphaned_nested(store, session, &orphans, ids, clock)
        .map_err(|reason| format!("its lost tool runs could not be recorded: {reason}"))?;
    crate::server_log::info(format_args!(
        "session {session}: recorded {} tool runs lost at restart as unknown",
        orphans.len()
    ));
    Ok(())
}
