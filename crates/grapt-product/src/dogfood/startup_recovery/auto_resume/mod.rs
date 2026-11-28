//! Bounded, idempotent automatic resume for crash-interrupted turns.
//!
//! Startup recovery records eligible interrupted sessions into `pending_resume`
//! in O(1) per recovered session (never scanning idle sessions). After server
//! readiness, `PromptDriver` runs `prompt_submission::auto_resume::drive` in
//! bounded pages: each eligible session has its attempt counter durably committed before launching a
//! synthetic resume turn with a collision-free deterministic `MessageId`.
//!
//! Non-resumable turns are never auto-resumed:
//! - Sessions with an unresolved or quarantined provider attempt, or an
//!   in-flight provider `ProcessLoss` step failure (Law D1).
//! - Child/subagent sessions, or parent sessions with active/lost subagent
//!   children.
//! - Sessions without a durable human prompt for the interrupted execution
//!   (excluding `seat-startup` and unpromoted queued inputs).
//! - Sessions with an open/suspended permission request, pending form, or
//!   pre-recovery interactive question.
//! - Sessions that reached [`MAX_AUTO_RESUME_ATTEMPTS`] for the same
//!   `turn_origin_seq` (preventing poison-turn crash loops).

mod store;
#[cfg(test)]
mod tests;

use std::collections::HashSet;

use grapt_service_state::{Action, Change, Kind, Reason, State};
use grapt_store::{EventStore, GovernedTransaction, OsRandomIds, SystemClock};
use grapt_types::{ClockAuthority, DeterministicAuthorities, SessionId};

use self::store::{
    has_active_or_lost_subagent_child, has_ambiguous_process_loss_step,
    has_pending_approval_or_form, has_unresolved_or_quarantined_attempt, interrupted_turn,
    is_child_session, latest_human_input_seq,
};
pub(in crate::dogfood) use self::store::{
    list_runnable_after, load_row, synthetic_input_progress, synthetic_message_id, upsert_row,
    upsert_row_with_input, PendingRow, ResumeState, StoredRow, SyntheticProgress,
};

/// Maximum automatic resume attempts for a single user turn (`turn_origin_seq`).
pub(crate) const MAX_AUTO_RESUME_ATTEMPTS: u32 = 2;
/// Sessions loaded per page during post-readiness automatic resume.
pub(crate) const AUTO_RESUME_PAGE_SIZE: usize = 32;

pub(in crate::dogfood) fn ensure_schema_within(tx: &GovernedTransaction<'_>) -> Result<(), String> {
    store::ensure_schema_within(tx)
}

pub(super) fn ensure_schema_if_missing(store: &mut EventStore) -> Result<(), String> {
    store::ensure_schema_if_missing(store)
}

/// Durably fence sessions that have pending forms or interactive questions
/// BEFORE `elicitation::recover` cancels them so even a second crash between
/// form cancellation and status recovery cannot erase the blocker. Performs
/// zero writes when no pending session forms exist.
pub(super) fn fence_pre_recovery_form_sessions(
    store: &mut EventStore,
    clock: &SystemClock,
) -> Result<HashSet<SessionId>, String> {
    let sessions = store::collect_pre_recovery_form_sessions(store.connection())?;
    if sessions.is_empty() {
        return Ok(sessions);
    }
    let now_ms = clock.now().as_millis();
    for session in &sessions {
        let snapshot = grapt_service_state::read(store.connection(), session)?;
        let Some(execution) = snapshot.execution else {
            continue;
        };
        let existing = load_row(store.connection(), session)?;
        let turn_origin_seq = interrupted_turn(store.connection(), session, &execution)?
            .map(|turn| turn.origin_seq)
            .or_else(|| existing.as_ref().map(|row| row.turn_origin_seq))
            .unwrap_or(0);
        let attempts = existing
            .as_ref()
            .filter(|row| row.turn_origin_seq == turn_origin_seq)
            .map_or(0, |row| row.attempts);
        upsert_row(
            store,
            session,
            turn_origin_seq,
            execution.as_str(),
            attempts,
            ResumeState::Stopped,
            Some("pending_approval_or_form"),
            now_ms,
        )?;
    }
    Ok(sessions)
}

/// Evaluate whether an interrupted session (holding its writer, right before
/// `grapt_service_state::recover` records `Disconnected`) should be queued for
/// automatic resume after readiness.
pub(super) fn enqueue_if_eligible(
    store: &mut EventStore,
    session: &SessionId,
    pre_recovery_forms: &HashSet<SessionId>,
    clock: &SystemClock,
    ids: &OsRandomIds,
) -> Result<(), String> {
    let snapshot = grapt_service_state::read(store.connection(), session)?;
    if !snapshot.state.active() {
        return Ok(());
    }
    let Some(execution) = snapshot.execution else {
        return Ok(());
    };
    if is_child_session(store.connection(), session)? {
        return Ok(());
    }
    let Some(turn) = interrupted_turn(store.connection(), session, &execution)? else {
        return Ok(());
    };
    let turn_origin_seq = turn.origin_seq;
    let now_ms = clock.now().as_millis();
    let existing = load_row(store.connection(), session)?;
    if existing.as_ref().is_some_and(|row| {
        row.state == ResumeState::Stopped
            && row.reason.as_deref() == Some("pending_approval_or_form")
            && row.interrupted_execution_id == execution.as_str()
    }) {
        crate::server_log::info(format_args!(
            "session {session}: automatic resume skipped reason=pending-approval-or-form-fenced"
        ));
        return Ok(());
    }
    let attempts = existing
        .as_ref()
        .filter(|row| row.turn_origin_seq == turn_origin_seq)
        .map_or(0, |row| row.attempts);

    if let Some(reason) = ineligible_reason(
        store.connection(),
        session,
        turn.execution_start_seq,
        pre_recovery_forms,
    )? {
        upsert_row(
            store,
            session,
            turn_origin_seq,
            execution.as_str(),
            attempts,
            ResumeState::Stopped,
            Some(reason),
            now_ms,
        )?;
        crate::server_log::info(format_args!(
            "session {session}: automatic resume skipped reason={reason}"
        ));
        return Ok(());
    }

    if attempts >= MAX_AUTO_RESUME_ATTEMPTS {
        upsert_row(
            store,
            session,
            turn_origin_seq,
            execution.as_str(),
            attempts,
            ResumeState::Stopped,
            Some("max_auto_resume_attempts_exceeded"),
            now_ms,
        )?;
        if let Err(error) = record_cap_status(store, clock, ids, session) {
            crate::server_log::warn(format_args!(
                "session {session}: failed to record automatic resume cap status: {error}"
            ));
            return Err(error);
        }
        crate::server_log::warn(format_args!(
            "session {session}: automatic resume stopped after {attempts} attempts (cap={MAX_AUTO_RESUME_ATTEMPTS})"
        ));
        return Ok(());
    }

    upsert_row(
        store,
        session,
        turn_origin_seq,
        execution.as_str(),
        attempts,
        ResumeState::Pending,
        None,
        now_ms,
    )?;
    crate::server_log::info(format_args!(
        "session {session}: queued interrupted turn for automatic resume origin_seq={turn_origin_seq} attempts={attempts}/{MAX_AUTO_RESUME_ATTEMPTS}"
    ));
    Ok(())
}

fn ineligible_reason(
    conn: &rusqlite::Connection,
    session: &SessionId,
    execution_start_seq: u64,
    pre_recovery_forms: &HashSet<SessionId>,
) -> Result<Option<&'static str>, String> {
    if has_unresolved_or_quarantined_attempt(conn, session)? {
        return Ok(Some("unresolved_or_quarantined_attempt"));
    }
    if has_ambiguous_process_loss_step(conn, session, execution_start_seq)? {
        return Ok(Some("ambiguous_provider_attempt"));
    }
    if pre_recovery_forms.contains(session) || has_pending_approval_or_form(conn, session)? {
        return Ok(Some("pending_approval_or_form"));
    }
    if has_active_or_lost_subagent_child(conn, session)? {
        return Ok(Some("pending_subagent_child"));
    }
    Ok(None)
}

/// Atomic pre-admission guard called under `ServerApplication::lock_inbox()`
/// right before `admit_synthetic`, preventing a race with a concurrent user
/// prompt or newly-active execution.
pub(in crate::dogfood) fn verify_before_admission(
    conn: &rusqlite::Connection,
    session: &SessionId,
    turn_origin_seq: u64,
) -> Result<Option<&'static str>, String> {
    let status = grapt_service_state::read(conn, session)?;
    if status.state.active() {
        return Ok(Some("superseded_by_active_execution"));
    }
    if let Some(latest) = latest_human_input_seq(conn, session)? {
        if latest > turn_origin_seq {
            return Ok(Some("superseded_by_newer_prompt"));
        }
    }
    if has_pending_approval_or_form(conn, session)? {
        return Ok(Some("pending_approval_or_form"));
    }
    if has_active_or_lost_subagent_child(conn, session)? {
        return Ok(Some("pending_subagent_child"));
    }
    Ok(None)
}

/// Atomically settle an open automatic-resume claim and its active synthetic
/// resume binding inside the execution terminal's governed transaction.
///
/// Custody-tied: only settles when the completing execution's owned `input`
/// matches the active resume attempt (or a newer human input that superseded
/// `turn_origin_seq`), never for an unrelated or unowned wake.
pub(in crate::dogfood) fn settle_for_execution_within(
    tx: &GovernedTransaction<'_>,
    writer: grapt_store::FencedWriter,
    session: &SessionId,
    input: Option<&grapt_types::MessageId>,
    outcome: grapt_session::execution::Outcome,
    now_ms: i64,
) -> Result<usize, String> {
    let Some(input) = input else {
        return Ok(0);
    };
    if !store::table_exists(tx, "pending_resume")? {
        return Ok(0);
    }
    let Some(current) = load_row(tx, session)? else {
        return Ok(0);
    };
    if !matches!(current.state, ResumeState::Pending | ResumeState::Running) {
        return Ok(0);
    }
    let expected_resume_id =
        synthetic_message_id(session, current.turn_origin_seq, current.attempts)?;
    let matches_resume = *input == expected_resume_id;
    let superseded_by_newer_human = !matches_resume
        && store::is_newer_human_input_within(tx, session, input, current.turn_origin_seq)?;
    if !matches_resume && !superseded_by_newer_human {
        return Ok(0);
    }
    let (next_state, reason) = if superseded_by_newer_human {
        (ResumeState::Completed, "superseded_by_completed_execution")
    } else {
        match outcome {
            grapt_session::execution::Outcome::Succeeded => {
                (ResumeState::Completed, "execution_succeeded")
            }
            grapt_session::execution::Outcome::Failed => (ResumeState::Stopped, "execution_failed"),
            grapt_session::execution::Outcome::Interrupted => {
                (ResumeState::Stopped, "execution_interrupted")
            }
        }
    };
    store::upsert_row_within(
        tx,
        writer,
        session,
        current.turn_origin_seq,
        &current.interrupted_execution_id,
        current.attempts,
        next_state,
        Some(reason),
        now_ms,
        None,
    )
}

/// Record a durable `Disconnected` status notice explaining that automatic
/// resume stopped due to the retry cap.
pub(in crate::dogfood) fn record_cap_status(
    store: &mut EventStore,
    clock: &SystemClock,
    ids: &OsRandomIds,
    session: &SessionId,
) -> Result<(), String> {
    let snapshot = grapt_service_state::read(store.connection(), session)?;
    let Some(execution) = snapshot.execution else {
        return Ok(());
    };
    let aggregate = grapt_store::AggregateId::Session(session.clone());
    let held = store.fencing_token(&aggregate).is_ok();
    if !held {
        store
            .elect_writer(&aggregate)
            .map_err(|error| format!("cap status writer election failed: {error}"))?;
    }
    let authorities = DeterministicAuthorities::new(clock, ids);
    let message = format!(
        "Automatic resume stopped after reaching the retry limit ({MAX_AUTO_RESUME_ATTEMPTS}); work so far is kept. Send a follow-up prompt to continue."
    );
    let result = grapt_service_state::record(
        store,
        &authorities,
        session,
        &execution,
        State::described(
            Kind::Disconnected,
            Reason::ProcessLoss,
            &message,
            Some(Action::FollowUp),
        ),
        Change::Recover,
    );
    if !held {
        store.release_held_writer(&aggregate);
    }
    result.map(|_| ())
}
