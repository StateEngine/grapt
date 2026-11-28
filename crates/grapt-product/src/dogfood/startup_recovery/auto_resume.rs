//! Bounded, idempotent automatic resume for crash-interrupted turns.
//!
//! Startup recovery records eligible interrupted sessions into `pending_resume`
//! in O(1) per recovered session (never scanning idle sessions). After server
//! readiness, `PromptDriver` runs [`drive`] in bounded pages: each eligible
//! session has its attempt counter durably incremented BEFORE launching a
//! synthetic resume turn with a deterministic `MessageId`.
//!
//! Non-resumable turns are never auto-resumed:
//! - Sessions with an ambiguous in-flight provider `attempt_receipt` (Law D1).
//! - Child/subagent sessions (cancelled on process loss by subagent adoption).
//! - Sessions without a durable human prompt.
//! - Sessions with an open or suspended permission request or pending form.
//! - Sessions that have already reached [`MAX_AUTO_RESUME_ATTEMPTS`] for the
//!   same `turn_origin_seq` (preventing poison-turn crash loops).

use super::*;
use grapt_api::SealedPrincipal;
use grapt_service_state::{Action, Change, Kind, Reason, State};
use grapt_store::{EventStore, GovernedTransaction};
use grapt_types::{ClockAuthority, DeterministicAuthorities, MessageId, SessionId};

/// Maximum automatic resume attempts for a single user turn (`turn_origin_seq`).
pub(crate) const MAX_AUTO_RESUME_ATTEMPTS: u32 = 2;
/// Sessions loaded per page during post-readiness automatic resume.
pub(crate) const AUTO_RESUME_PAGE_SIZE: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResumeState {
    Pending,
    Running,
    Completed,
    Stopped,
}

impl ResumeState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Stopped => "stopped",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "stopped" => Some(Self::Stopped),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingRow {
    session: SessionId,
    turn_origin_seq: u64,
    interrupted_execution_id: String,
    attempts: u32,
}

pub(in crate::dogfood) fn ensure_schema_within(tx: &GovernedTransaction<'_>) -> Result<(), String> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS pending_resume (
                session_id TEXT PRIMARY KEY NOT NULL,
                turn_origin_seq INTEGER NOT NULL,
                interrupted_execution_id TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                state TEXT NOT NULL,
                reason TEXT,
                updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_pending_resume_state_session
                ON pending_resume (state, session_id);",
    )
    .map_err(|error| format!("pending_resume schema failed: {error}"))
}

pub(in crate::dogfood) fn ensure_schema(store: &mut EventStore) -> Result<(), String> {
    let tx = store
        .governed_transaction()
        .map_err(|error| format!("pending_resume schema transaction refused: {error}"))?;
    ensure_schema_within(&tx)?;
    tx.commit()
        .map_err(|error| format!("pending_resume schema commit failed: {error}"))?
        .discard();
    Ok(())
}

/// Evaluate whether an interrupted session (holding its writer, right before
/// `grapt_service_state::recover` records `Disconnected`) should be queued for
/// automatic resume after readiness.
pub(super) fn enqueue_if_eligible(
    store: &mut EventStore,
    session: &SessionId,
    clock: &SystemClock,
    ids: &OsRandomIds,
) -> Result<(), String> {
    let snapshot = grapt_service_state::read(store.connection(), session)?;
    if !snapshot.state.active() {
        return Ok(());
    }
    let execution = match snapshot.execution {
        Some(execution) => execution,
        None => return Ok(()),
    };
    if is_child_session(store.connection(), session)? {
        return Ok(());
    }
    let Some(turn_origin_seq) = latest_human_input_seq(store.connection(), session)? else {
        return Ok(());
    };
    let now_ms = clock.now().as_millis();
    let existing = load_row(store.connection(), session)?;
    let attempts = existing
        .as_ref()
        .filter(|row| row.turn_origin_seq == turn_origin_seq)
        .map_or(0, |row| row.attempts);
    if has_process_loss_step_failure_since(store.connection(), session, turn_origin_seq)? {
        upsert_row(
            store,
            session,
            turn_origin_seq,
            execution.as_str(),
            attempts,
            ResumeState::Stopped,
            Some("ambiguous_provider_attempt"),
            now_ms,
        )?;
        crate::server_log::info(format_args!(
            "session {session}: automatic resume skipped reason=ambiguous-provider-attempt"
        ));
        return Ok(());
    }
    if has_pending_approval_or_form(store.connection(), session)? {
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
        crate::server_log::info(format_args!(
            "session {session}: automatic resume skipped reason=pending-approval-or-form"
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
        record_cap_status(store, clock, ids, session);
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

/// Background post-readiness driver: visits `pending_resume` rows in bounded
/// pages, claims each row durably (incrementing `attempts`) before launching,
/// and submits a deterministic synthetic resume prompt.
pub(in crate::dogfood) async fn drive(
    application: Arc<DogfoodApplication>,
    principal: SealedPrincipal,
) {
    let mut after = String::new();
    loop {
        let page = {
            let inbox = application.lock_inbox();
            match list_pending_after(inbox.store().connection(), &after, AUTO_RESUME_PAGE_SIZE) {
                Ok(page) => page,
                Err(error) => {
                    crate::server_log::warn(format_args!(
                        "auto-resume warning reason=page-read-failed detail={error}"
                    ));
                    return;
                }
            }
        };
        let Some(last) = page.last() else {
            return;
        };
        after = last.session.as_str().to_owned();
        for row in page {
            resume_one(&application, &principal, row).await;
        }
    }
}

enum ClaimOutcome {
    Launch { attempt: u32 },
    Skip,
}

async fn resume_one(
    application: &Arc<DogfoodApplication>,
    principal: &SealedPrincipal,
    row: PendingRow,
) {
    let claimed = match claim_for_launch(application, &row) {
        Ok(outcome) => outcome,
        Err(error) => {
            crate::server_log::warn(format_args!(
                "session {}: auto-resume claim skipped detail={error}",
                row.session
            ));
            return;
        }
    };
    let ClaimOutcome::Launch { attempt } = claimed else {
        return;
    };
    let Ok(input) = resume_message_id(&row.session, row.turn_origin_seq, attempt) else {
        crate::server_log::warn(format_args!(
            "session {}: auto-resume message id invalid",
            row.session
        ));
        return;
    };
    let text = "The service restarted before this turn finished; committed work and tool outcomes so far are preserved. Continue from the last committed step.".to_owned();
    let description = Some(format!(
        "Automatic resume after restart (attempt {attempt} of {MAX_AUTO_RESUME_ATTEMPTS})"
    ));
    let metadata = Some(json!({
        "autoResume": {
            "turnOriginSeq": row.turn_origin_seq,
            "interruptedExecutionID": row.interrupted_execution_id,
            "attempt": attempt,
            "maxAttempts": MAX_AUTO_RESUME_ATTEMPTS,
        }
    }));
    crate::server_log::info(format_args!(
        "session {}: auto-resuming interrupted turn origin_seq={} attempt={attempt}/{MAX_AUTO_RESUME_ATTEMPTS}",
        row.session, row.turn_origin_seq
    ));
    match application
        .submit_synthetic(
            principal.clone(),
            row.session.clone(),
            input,
            text,
            description,
            metadata,
            DeliveryMode::Queue,
            true,
        )
        .await
    {
        Ok(_) => {
            let now_ms = application.clock.now().as_millis();
            let mut inbox = application.lock_inbox();
            let _ = mark_state_if_matching(
                inbox.store_mut(),
                &row.session,
                row.turn_origin_seq,
                attempt,
                ResumeState::Completed,
                None,
                now_ms,
            );
        }
        Err(error) => {
            crate::server_log::warn(format_args!(
                "session {}: auto-resume launch refused detail={error}",
                row.session
            ));
            let now_ms = application.clock.now().as_millis();
            let mut inbox = application.lock_inbox();
            let _ = mark_state_if_matching(
                inbox.store_mut(),
                &row.session,
                row.turn_origin_seq,
                attempt,
                ResumeState::Stopped,
                Some(&bounded_reason(&error)),
                now_ms,
            );
        }
    }
}

fn claim_for_launch(
    application: &Arc<DogfoodApplication>,
    row: &PendingRow,
) -> Result<ClaimOutcome, String> {
    let active = application
        .executor
        .active_executions()
        .iter()
        .any(|(session, _)| session == &row.session);
    let now_ms = application.clock.now().as_millis();
    let mut inbox = application.lock_inbox();
    let store = inbox.store_mut();
    let Some(current) = load_row(store.connection(), &row.session)? else {
        return Ok(ClaimOutcome::Skip);
    };
    if current.state != ResumeState::Pending || current.turn_origin_seq != row.turn_origin_seq {
        return Ok(ClaimOutcome::Skip);
    }
    if active {
        upsert_row(
            store,
            &row.session,
            current.turn_origin_seq,
            &current.interrupted_execution_id,
            current.attempts,
            ResumeState::Completed,
            Some("superseded_by_active_execution"),
            now_ms,
        )?;
        return Ok(ClaimOutcome::Skip);
    }
    if let Some(latest) = latest_human_input_seq(store.connection(), &row.session)? {
        if latest > current.turn_origin_seq {
            upsert_row(
                store,
                &row.session,
                current.turn_origin_seq,
                &current.interrupted_execution_id,
                current.attempts,
                ResumeState::Completed,
                Some("superseded_by_newer_prompt"),
                now_ms,
            )?;
            return Ok(ClaimOutcome::Skip);
        }
    }
    if has_pending_approval_or_form(store.connection(), &row.session)? {
        upsert_row(
            store,
            &row.session,
            current.turn_origin_seq,
            &current.interrupted_execution_id,
            current.attempts,
            ResumeState::Stopped,
            Some("pending_approval_or_form"),
            now_ms,
        )?;
        return Ok(ClaimOutcome::Skip);
    }
    if current.attempts >= MAX_AUTO_RESUME_ATTEMPTS {
        upsert_row(
            store,
            &row.session,
            current.turn_origin_seq,
            &current.interrupted_execution_id,
            current.attempts,
            ResumeState::Stopped,
            Some("max_auto_resume_attempts_exceeded"),
            now_ms,
        )?;
        record_cap_status(store, &application.clock, &application.ids, &row.session);
        return Ok(ClaimOutcome::Skip);
    }
    let next_attempt = current.attempts.saturating_add(1);
    upsert_row(
        store,
        &row.session,
        current.turn_origin_seq,
        &current.interrupted_execution_id,
        next_attempt,
        ResumeState::Running,
        None,
        now_ms,
    )?;
    Ok(ClaimOutcome::Launch {
        attempt: next_attempt,
    })
}

/// Record a durable `Disconnected` status notice explaining that automatic
/// resume stopped due to the retry cap (best-effort if execution id is present).
pub(super) fn record_cap_status(
    store: &mut EventStore,
    clock: &SystemClock,
    ids: &OsRandomIds,
    session: &SessionId,
) {
    let Ok(snapshot) = grapt_service_state::read(store.connection(), session) else {
        return;
    };
    let Some(execution) = snapshot.execution else {
        return;
    };
    let aggregate = grapt_store::AggregateId::Session(session.clone());
    let held = store.fencing_token(&aggregate).is_ok();
    if !held && store.elect_writer(&aggregate).is_err() {
        return;
    }
    let authorities = DeterministicAuthorities::new(clock, ids);
    let message = format!(
        "Automatic resume stopped after reaching the retry limit ({MAX_AUTO_RESUME_ATTEMPTS}); work so far is kept. Send a follow-up prompt to continue."
    );
    let _ = grapt_service_state::record(
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredRow {
    turn_origin_seq: u64,
    interrupted_execution_id: String,
    attempts: u32,
    state: ResumeState,
    reason: Option<String>,
}

fn load_row(conn: &rusqlite::Connection, session: &SessionId) -> Result<Option<StoredRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT turn_origin_seq, interrupted_execution_id, attempts, state, reason
             FROM pending_resume WHERE session_id = ?1",
        )
        .map_err(|error| format!("pending_resume read prepare failed: {error}"))?;
    let mut rows = stmt
        .query([session.as_str()])
        .map_err(|error| format!("pending_resume read failed: {error}"))?;
    let Some(row) = rows
        .next()
        .map_err(|error| format!("pending_resume row failed: {error}"))?
    else {
        return Ok(None);
    };
    let turn_origin_seq = u64::try_from(
        row.get::<_, i64>(0)
            .map_err(|error| format!("pending_resume turn_origin_seq failed: {error}"))?,
    )
    .map_err(|_| "pending_resume turn_origin_seq negative".to_owned())?;
    let interrupted_execution_id: String = row
        .get(1)
        .map_err(|error| format!("pending_resume execution failed: {error}"))?;
    let attempts = u32::try_from(
        row.get::<_, i64>(2)
            .map_err(|error| format!("pending_resume attempts failed: {error}"))?,
    )
    .map_err(|_| "pending_resume attempts negative".to_owned())?;
    let state_raw: String = row
        .get(3)
        .map_err(|error| format!("pending_resume state failed: {error}"))?;
    let state = ResumeState::parse(&state_raw)
        .ok_or_else(|| format!("pending_resume unknown state: {state_raw}"))?;
    let reason: Option<String> = row
        .get(4)
        .map_err(|error| format!("pending_resume reason failed: {error}"))?;
    Ok(Some(StoredRow {
        turn_origin_seq,
        interrupted_execution_id,
        attempts,
        state,
        reason,
    }))
}

#[allow(clippy::too_many_arguments, reason = "pending_resume row columns")]
fn upsert_row(
    store: &mut EventStore,
    session: &SessionId,
    turn_origin_seq: u64,
    interrupted_execution_id: &str,
    attempts: u32,
    state: ResumeState,
    reason: Option<&str>,
    updated_at: i64,
) -> Result<(), String> {
    let seq = i64::try_from(turn_origin_seq)
        .map_err(|_| "pending_resume turn_origin_seq exceeds i64".to_owned())?;
    let tx = store
        .governed_transaction()
        .map_err(|error| format!("pending_resume transaction refused: {error}"))?;
    tx.execute(
            "INSERT INTO pending_resume
                (session_id, turn_origin_seq, interrupted_execution_id, attempts, state, reason, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(session_id) DO UPDATE SET
                turn_origin_seq = excluded.turn_origin_seq,
                interrupted_execution_id = excluded.interrupted_execution_id,
                attempts = excluded.attempts,
                state = excluded.state,
                reason = excluded.reason,
                updated_at = excluded.updated_at",
            rusqlite::params![
                session.as_str(),
                seq,
                interrupted_execution_id,
                i64::from(attempts),
                state.as_str(),
                reason,
                updated_at,
            ],
        )
        .map_err(|error| format!("pending_resume upsert failed: {error}"))?;
    tx.commit()
        .map_err(|error| format!("pending_resume commit failed: {error}"))?
        .discard();
    Ok(())
}

fn mark_state_if_matching(
    store: &mut EventStore,
    session: &SessionId,
    turn_origin_seq: u64,
    attempts: u32,
    state: ResumeState,
    reason: Option<&str>,
    updated_at: i64,
) -> Result<(), String> {
    let seq = i64::try_from(turn_origin_seq)
        .map_err(|_| "pending_resume turn_origin_seq exceeds i64".to_owned())?;
    let tx = store
        .governed_transaction()
        .map_err(|error| format!("pending_resume update transaction refused: {error}"))?;
    tx.execute(
            "UPDATE pending_resume
             SET state = ?1, reason = ?2, updated_at = ?3
             WHERE session_id = ?4 AND turn_origin_seq = ?5 AND attempts = ?6 AND state = 'running'",
            rusqlite::params![
                state.as_str(),
                reason,
                updated_at,
                session.as_str(),
                seq,
                i64::from(attempts),
            ],
        )
        .map_err(|error| format!("pending_resume update failed: {error}"))?;
    tx.commit()
        .map_err(|error| format!("pending_resume update commit failed: {error}"))?
        .discard();
    Ok(())
}

fn list_pending_after(
    conn: &rusqlite::Connection,
    after: &str,
    limit: usize,
) -> Result<Vec<PendingRow>, String> {
    let limit = i64::try_from(limit.max(1)).map_err(|error| error.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT session_id, turn_origin_seq, interrupted_execution_id, attempts
             FROM pending_resume
             WHERE state = 'pending' AND session_id > ?1
             ORDER BY session_id
             LIMIT ?2",
        )
        .map_err(|error| format!("pending_resume list prepare failed: {error}"))?;
    let rows = stmt
        .query_map(rusqlite::params![after, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|error| format!("pending_resume list failed: {error}"))?;
    let mut out = Vec::new();
    for entry in rows {
        let (raw_session, seq, execution, attempts) =
            entry.map_err(|error| format!("pending_resume list row failed: {error}"))?;
        let session = raw_session
            .parse::<SessionId>()
            .map_err(|error| format!("pending_resume session id invalid: {error}"))?;
        let turn_origin_seq =
            u64::try_from(seq).map_err(|_| "pending_resume turn_origin_seq negative".to_owned())?;
        let attempts =
            u32::try_from(attempts).map_err(|_| "pending_resume attempts negative".to_owned())?;
        out.push(PendingRow {
            session,
            turn_origin_seq,
            interrupted_execution_id: execution,
            attempts,
        });
    }
    Ok(out)
}

fn latest_human_input_seq(
    conn: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Option<u64>, String> {
    let seq: Option<i64> = conn
        .query_row(
            "SELECT MAX(seq) FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type = 'session.input.admitted.1'
               AND CASE WHEN json_valid(data)
                   THEN COALESCE(
                       json_extract(data, '$.input.type'),
                       json_extract(data, '$.kind'),
                       'user'
                   ) = 'user'
                   ELSE 0
               END",
            [session.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| format!("human input lookup failed: {error}"))?;
    seq.map(|value| u64::try_from(value).map_err(|_| "human input seq negative".to_owned()))
        .transpose()
}

fn is_child_session(conn: &rusqlite::Connection, session: &SessionId) -> Result<bool, String> {
    let parent: Option<String> = conn
        .query_row(
            "SELECT CASE WHEN json_valid(data) THEN json_extract(data, '$.info.parentID') END
             FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type = 'session.created.1'
             ORDER BY seq ASC LIMIT 1",
            [session.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("child session lookup failed: {error}"))?
        .flatten();
    Ok(parent.is_some_and(|value| !value.is_empty()))
}

/// True if the latest `ProcessLoss` step failure since `since_seq` occurred
/// while a provider HTTP request was still in flight (no `session.tool.%`
/// event committed in that step). When a tool call already started/settled
/// inside that step, the provider response had already arrived and the turn is
/// safe to auto-resume once lost tool runs are settled.
fn has_process_loss_step_failure_since(
    conn: &rusqlite::Connection,
    session: &SessionId,
    since_seq: u64,
) -> Result<bool, String> {
    let since_seq =
        i64::try_from(since_seq).map_err(|_| "turn_origin_seq exceeds i64".to_owned())?;
    let fail_seq: Option<i64> = conn
        .query_row(
            "SELECT MAX(seq) FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND seq > ?2
               AND type = 'session.step.failed.2'
               AND (data LIKE '%ProcessLoss%'
                    OR data LIKE '%process_loss%'
                    OR data LIKE '%process-loss%')",
            rusqlite::params![session.as_str(), since_seq],
            |row| row.get(0),
        )
        .map_err(|error| format!("process-loss step failure lookup failed: {error}"))?;
    let Some(fail_seq) = fail_seq else {
        return Ok(false);
    };
    let step_start_seq: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(seq), ?2) FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND seq > ?2 AND seq < ?3
               AND type LIKE 'session.step.started.%'",
            rusqlite::params![session.as_str(), since_seq, fail_seq],
            |row| row.get(0),
        )
        .map_err(|error| format!("step start lookup failed: {error}"))?;
    let tool_events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND seq > ?2 AND seq < ?3
               AND type LIKE 'session.tool.%'",
            rusqlite::params![session.as_str(), step_start_seq, fail_seq],
            |row| row.get(0),
        )
        .map_err(|error| format!("step tool event lookup failed: {error}"))?;
    Ok(tool_events == 0)
}

fn has_pending_approval_or_form(
    conn: &rusqlite::Connection,
    session: &SessionId,
) -> Result<bool, String> {
    let open_permission: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM permission_request
             WHERE session_id = ?1 AND status IN ('pending', 'suspended')",
            [session.as_str()],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if open_permission > 0 {
        return Ok(true);
    }
    let open_form: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM form_request
             WHERE session_id = ?1 AND status = 'pending'",
            [session.as_str()],
            |row| row.get(0),
        )
        .unwrap_or(0);
    Ok(open_form > 0)
}

fn resume_message_id(
    session: &SessionId,
    turn_origin_seq: u64,
    attempt: u32,
) -> Result<MessageId, String> {
    let sanitized: String = session
        .as_str()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
        .take(24)
        .collect();
    format!("msg_autoresume_{sanitized}_{turn_origin_seq}_{attempt}")
        .parse()
        .map_err(|_| "invalid auto-resume message id".to_owned())
}

fn bounded_reason(reason: &str) -> String {
    let mut out = String::with_capacity(reason.len().min(256));
    for ch in reason.chars() {
        let ch = if ch.is_control() { ' ' } else { ch };
        if out.len() + ch.len_utf8() > 256 {
            break;
        }
        out.push(ch);
    }
    out
}

use rusqlite::OptionalExtension as _;

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_event::AggregateId;
    use grapt_store::{AppendInput, Appended};
    use grapt_types::{EventId, EventVersion, ExecutionId};

    fn test_store() -> Result<(EventStore, OsRandomIds, SystemClock), String> {
        let mut store =
            EventStore::open_in_memory().map_err(|error| format!("open store: {error}"))?;
        grapt_form::FormService::initialize(&mut store).map_err(|error| error.to_string())?;
        PermissionHttpState::initialize(&mut store).map_err(|error| error.to_string())?;
        ensure_schema(&mut store)?;
        let ids = OsRandomIds::seed().map_err(|error| error.to_string())?;
        Ok((store, ids, SystemClock))
    }

    fn append_event(
        store: &mut EventStore,
        ids: &OsRandomIds,
        clock: &SystemClock,
        session: &SessionId,
        kind_name: &str,
        data: serde_json::Value,
    ) -> Result<u64, String> {
        let aggregate = AggregateId::Session(session.clone());
        let writer = match store.fencing_token(&aggregate) {
            Ok(writer) => writer,
            Err(_) => {
                store
                    .elect_writer(&aggregate)
                    .map_err(|error| error.to_string())?;
                store
                    .fencing_token(&aggregate)
                    .map_err(|error| error.to_string())?
            }
        };
        let id: EventId = format!("evt_{}", ids.mint().as_str())
            .parse()
            .map_err(|_| "event id".to_owned())?;
        let version = EventVersion::new(1).ok_or("event version")?;
        let (appended, _) = store
            .append(
                AppendInput {
                    id: &id,
                    aggregate,
                    writer,
                    kind_name,
                    version,
                    created_ms: clock.now().as_millis(),
                    data: &data,
                },
                &mut [],
            )
            .map_err(|error| error.to_string())?;
        let seq = match appended {
            Appended::Committed { seq } | Appended::Idempotent { seq } => seq,
        };
        Ok(seq)
    }

    fn seed_active_turn(
        store: &mut EventStore,
        ids: &OsRandomIds,
        clock: &SystemClock,
        session: &SessionId,
        parent: Option<&str>,
        exec_name: &str,
    ) -> Result<u64, String> {
        let authorities = DeterministicAuthorities::new(clock, ids);
        let directory = std::env::temp_dir();
        let directory = directory.to_str().ok_or("temp dir not utf8")?;
        let project = grapt_types::ProjectId::new("prj_auto_resume").ok_or("project")?;
        let mut seed = grapt_session::SessionSeed::new(project, directory, "auto-resume")
            .map_err(|error| error.to_string())?
            .with_requested_id(session.clone())
            .map_err(|error| error.to_string())?;
        if let Some(parent) = parent {
            let parent_id: SessionId = parent.parse().map_err(|error| format!("{error}"))?;
            seed = seed.with_parent(parent_id);
        }
        grapt_session::create_session(store, &authorities, &seed, &mut [])
            .map_err(|error| error.to_string())?;
        let user_seq = append_event(
            store,
            ids,
            clock,
            session,
            "session.input.admitted",
            json!({"id": "msg_u1", "kind": "user", "text": "do the work"}),
        )?;
        let execution = ExecutionId::new(exec_name).ok_or("exec id")?;
        grapt_service_state::record(
            store,
            &authorities,
            session,
            &execution,
            State::plain(Kind::Running),
            Change::Start,
        )?;
        Ok(user_seq)
    }

    #[test]
    fn eligible_interrupted_turn_enqueues_and_respects_crash_loop_cap() -> Result<(), String> {
        let (mut store, ids, clock) = test_store()?;
        let session: SessionId = "ses_auto_resume_cap".parse().map_err(|e| format!("{e}"))?;
        let user_seq = seed_active_turn(&mut store, &ids, &clock, &session, None, "exec_1")?;

        enqueue_if_eligible(&mut store, &session, &clock, &ids)?;
        let row = load_row(store.connection(), &session)?.ok_or("missing pending_resume row")?;
        assert_eq!(row.state, ResumeState::Pending);
        assert_eq!(row.turn_origin_seq, user_seq);
        assert_eq!(row.attempts, 0);

        // Simulate two failed auto-resume attempts on the same turn_origin_seq.
        upsert_row(
            &mut store,
            &session,
            user_seq,
            "exec_2",
            MAX_AUTO_RESUME_ATTEMPTS,
            ResumeState::Running,
            None,
            clock.now().as_millis(),
        )?;
        enqueue_if_eligible(&mut store, &session, &clock, &ids)?;
        let capped = load_row(store.connection(), &session)?.ok_or("missing capped row")?;
        assert_eq!(capped.state, ResumeState::Stopped);
        assert_eq!(
            capped.reason.as_deref(),
            Some("max_auto_resume_attempts_exceeded")
        );
        assert_eq!(capped.attempts, MAX_AUTO_RESUME_ATTEMPTS);
        let status = grapt_service_state::read(store.connection(), &session)?;
        assert_eq!(status.state.kind, Kind::Disconnected);
        assert!(
            status
                .state
                .message
                .as_deref()
                .is_some_and(|msg| msg.contains("Automatic resume stopped")),
            "expected durable retry-limit message, got {:?}",
            status.state.message
        );

        // A brand new human prompt starts a fresh active turn_origin_seq with 0 attempts.
        let next_seq = append_event(
            &mut store,
            &ids,
            &clock,
            &session,
            "session.input.admitted",
            json!({"id": "msg_u2", "kind": "user", "text": "try again"}),
        )?;
        let authorities = DeterministicAuthorities::new(&clock, &ids);
        let exec_3 = ExecutionId::new("exec_3").ok_or("exec_3")?;
        grapt_service_state::record(
            &mut store,
            &authorities,
            &session,
            &exec_3,
            State::plain(Kind::Running),
            Change::Start,
        )?;
        enqueue_if_eligible(&mut store, &session, &clock, &ids)?;
        let fresh = load_row(store.connection(), &session)?.ok_or("missing fresh row")?;
        assert_eq!(fresh.state, ResumeState::Pending);
        assert_eq!(fresh.turn_origin_seq, next_seq);
        assert_eq!(fresh.attempts, 0);
        Ok(())
    }

    #[test]
    fn ambiguous_attempt_child_session_and_pending_approval_do_not_enqueue() -> Result<(), String> {
        let (mut store, ids, clock) = test_store()?;

        // 1. Ambiguous in-flight provider attempt (Law D1: step failed with ProcessLoss).
        let s_ambig: SessionId = "ses_auto_ambig".parse().map_err(|e| format!("{e}"))?;
        seed_active_turn(&mut store, &ids, &clock, &s_ambig, None, "exec_a")?;
        let aggregate = AggregateId::Session(s_ambig.clone());
        let writer = store
            .fencing_token(&aggregate)
            .map_err(|error| error.to_string())?;
        let event_id: EventId = format!("evt_{}", ids.mint().as_str())
            .parse()
            .map_err(|_| "event id".to_owned())?;
        let v2 = EventVersion::new(2).ok_or("v2")?;
        let (_, notices) = store
            .append(
                AppendInput {
                    id: &event_id,
                    aggregate,
                    writer,
                    kind_name: "session.step.failed",
                    version: v2,
                    created_ms: clock.now().as_millis(),
                    data: &json!({"reason": "process_loss"}),
                },
                &mut [],
            )
            .map_err(|error| error.to_string())?;
        notices.discard();
        enqueue_if_eligible(&mut store, &s_ambig, &clock, &ids)?;
        let row = load_row(store.connection(), &s_ambig)?.ok_or("missing ambig row")?;
        assert_eq!(row.state, ResumeState::Stopped);
        assert_eq!(row.reason.as_deref(), Some("ambiguous_provider_attempt"));

        // 2. Child/subagent session is skipped.
        let s_child: SessionId = "ses_auto_child".parse().map_err(|e| format!("{e}"))?;
        seed_active_turn(
            &mut store,
            &ids,
            &clock,
            &s_child,
            Some("ses_parent"),
            "exec_c",
        )?;
        enqueue_if_eligible(&mut store, &s_child, &clock, &ids)?;
        assert!(load_row(store.connection(), &s_child)?.is_none());

        // 3. Pending human permission blocks auto-resume.
        let s_perm: SessionId = "ses_auto_perm".parse().map_err(|e| format!("{e}"))?;
        seed_active_turn(&mut store, &ids, &clock, &s_perm, None, "exec_p")?;
        let tx = store
            .governed_transaction()
            .map_err(|error| error.to_string())?;
        tx.execute(
            "INSERT INTO permission_request (id, session_id, action, status)
             VALUES ('per_open_1', ?1, 'shell', 'pending')",
            [s_perm.as_str()],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?.discard();
        enqueue_if_eligible(&mut store, &s_perm, &clock, &ids)?;
        let row = load_row(store.connection(), &s_perm)?.ok_or("missing perm row")?;
        assert_eq!(row.state, ResumeState::Stopped);
        assert_eq!(row.reason.as_deref(), Some("pending_approval_or_form"));

        // 4. Paged listing returns only pending rows in bounded order.
        let s_ok: SessionId = "ses_auto_ok".parse().map_err(|e| format!("{e}"))?;
        seed_active_turn(&mut store, &ids, &clock, &s_ok, None, "exec_ok")?;
        enqueue_if_eligible(&mut store, &s_ok, &clock, &ids)?;
        let page = list_pending_after(store.connection(), "", 10)?;
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].session, s_ok);
        Ok(())
    }
}
