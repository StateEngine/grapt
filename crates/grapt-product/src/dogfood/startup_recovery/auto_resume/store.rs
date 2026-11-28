//! SQLite persistence and fail-closed eligibility queries for `pending_resume`.

use std::collections::HashSet;

use grapt_store::{EventStore, GovernedTransaction};
use grapt_types::{ExecutionId, MessageId, SessionId};
use rusqlite::OptionalExtension;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResumeState {
    Pending,
    Running,
    Completed,
    Stopped,
}

impl ResumeState {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Stopped => "stopped",
        }
    }

    pub(super) fn parse(raw: &str) -> Option<Self> {
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
pub(super) struct PendingRow {
    pub(super) session: SessionId,
    pub(super) turn_origin_seq: u64,
    pub(super) interrupted_execution_id: String,
    pub(super) attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoredRow {
    pub(super) turn_origin_seq: u64,
    pub(super) interrupted_execution_id: String,
    pub(super) attempts: u32,
    pub(super) state: ResumeState,
    pub(super) reason: Option<String>,
}

pub(super) fn ensure_schema_within(tx: &GovernedTransaction<'_>) -> Result<(), String> {
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

/// Install `pending_resume` only when upgrading a pre-existing store that lacks
/// it; fresh stores install it in `schemas::install_fresh` and steady restarts
/// perform zero writes.
pub(super) fn ensure_schema_if_missing(store: &mut EventStore) -> Result<(), String> {
    grapt_inbox::Inbox::ensure_tables(store.connection())
        .map_err(|error| format!("inbox schema setup failed: {error}"))?;
    if table_exists(store.connection(), "pending_resume")? {
        return Ok(());
    }
    let tx = store
        .governed_transaction()
        .map_err(|error| format!("pending_resume schema transaction refused: {error}"))?;
    ensure_schema_within(&tx)?;
    tx.commit()
        .map_err(|error| format!("pending_resume schema commit failed: {error}"))?
        .discard();
    Ok(())
}

pub(super) fn table_exists(conn: &rusqlite::Connection, table: &str) -> Result<bool, String> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
            [table],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("sqlite_master probe for {table} failed: {error}"))?;
    Ok(found.is_some())
}

pub(super) fn collect_pre_recovery_form_sessions(
    conn: &rusqlite::Connection,
) -> Result<HashSet<SessionId>, String> {
    if !table_exists(conn, "form_request")? {
        return Ok(HashSet::new());
    }
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT session_id FROM form_request
             WHERE status = 'pending' AND session_id IS NOT NULL",
        )
        .map_err(|error| format!("pre-recovery form scan prepare failed: {error}"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| format!("pre-recovery form scan failed: {error}"))?;
    let mut sessions = HashSet::new();
    for entry in rows {
        let raw = entry.map_err(|error| format!("pre-recovery form row failed: {error}"))?;
        let session = raw
            .parse::<SessionId>()
            .map_err(|error| format!("pre-recovery form session id invalid: {error}"))?;
        sessions.insert(session);
    }
    Ok(sessions)
}

pub(super) fn load_row(
    conn: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Option<StoredRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT turn_origin_seq, interrupted_execution_id, attempts, state, reason
             FROM pending_resume WHERE session_id = ?1",
        )
        .map_err(|error| format!("pending_resume read prepare failed: {error}"))?;
    let row = stmt
        .query_row([session.as_str()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .optional()
        .map_err(|error| format!("pending_resume read failed: {error}"))?;
    let Some((seq, execution, attempts, raw_state, reason)) = row else {
        return Ok(None);
    };
    let turn_origin_seq =
        u64::try_from(seq).map_err(|_| "pending_resume turn_origin_seq negative".to_owned())?;
    let attempts =
        u32::try_from(attempts).map_err(|_| "pending_resume attempts negative".to_owned())?;
    let state = ResumeState::parse(&raw_state)
        .ok_or_else(|| format!("pending_resume state invalid: {raw_state}"))?;
    Ok(Some(StoredRow {
        turn_origin_seq,
        interrupted_execution_id: execution,
        attempts,
        state,
        reason,
    }))
}

#[allow(clippy::too_many_arguments, reason = "pending_resume row columns")]
pub(super) fn upsert_row(
    store: &mut EventStore,
    session: &SessionId,
    turn_origin_seq: u64,
    interrupted_execution_id: &str,
    attempts: u32,
    state: ResumeState,
    reason: Option<&str>,
    updated_at: i64,
) -> Result<usize, String> {
    upsert_row_with_input(
        store,
        session,
        turn_origin_seq,
        interrupted_execution_id,
        attempts,
        state,
        reason,
        updated_at,
        None,
    )
}

#[allow(clippy::too_many_arguments, reason = "pending_resume row columns")]
pub(super) fn upsert_row_within(
    tx: &GovernedTransaction<'_>,
    writer: grapt_store::FencedWriter,
    session: &SessionId,
    turn_origin_seq: u64,
    interrupted_execution_id: &str,
    attempts: u32,
    state: ResumeState,
    reason: Option<&str>,
    updated_at: i64,
    active_input: Option<&MessageId>,
) -> Result<usize, String> {
    let seq = i64::try_from(turn_origin_seq)
        .map_err(|_| "turn_origin_seq overflows SQLite INTEGER".to_owned())?;
    let attempts = i64::from(attempts);
    let aggregate = grapt_store::AggregateId::Session(session.clone());
    EventStore::verify_writer_within(tx, &aggregate, writer)
        .map_err(|error| format!("pending_resume writer fence failed: {error}"))?;
    tx.execute(
        "INSERT INTO pending_resume (
             session_id, turn_origin_seq, interrupted_execution_id,
             attempts, state, reason, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
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
            attempts,
            state.as_str(),
            reason,
            updated_at
        ],
    )
    .map_err(|error| format!("pending_resume upsert failed: {error}"))?;
    if let Some(input_id) = active_input {
        grapt_inbox::Inbox::register_resume_input_within(
            tx,
            writer,
            session,
            input_id,
            turn_origin_seq,
        )
        .map_err(|error| format!("resume input registration failed: {error}"))?;
        Ok(0)
    } else if matches!(state, ResumeState::Completed | ResumeState::Stopped) {
        grapt_inbox::Inbox::retire_resume_inputs_within(tx, writer, session)
            .map_err(|error| format!("resume input retirement failed: {error}"))
    } else {
        Ok(0)
    }
}

/// True if `input` is a genuine human prompt (not `seat-startup`) admitted in
/// `session` strictly after `after_seq`.
pub(super) fn is_newer_human_input_within(
    tx: &GovernedTransaction<'_>,
    session: &SessionId,
    input: &MessageId,
    after_seq: u64,
) -> Result<bool, String> {
    let after_seq = i64::try_from(after_seq)
        .map_err(|_| "turn_origin_seq overflows SQLite INTEGER".to_owned())?;
    let found: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type = 'session.input.admitted.1'
               AND seq > ?2
               AND CASE WHEN json_valid(data)
                   THEN json_extract(data, '$.inputID') = ?3
                        AND COALESCE(json_extract(data, '$.input.type'), json_extract(data, '$.kind'), 'user') = 'user'
                        AND COALESCE(json_extract(data, '$.input.data.metadata.origin'), '') != 'seat-startup'
                   ELSE 0
               END
             LIMIT 1",
            rusqlite::params![session.as_str(), after_seq, input.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("human input ownership check failed: {error}"))?;
    Ok(found.is_some())
}

#[allow(clippy::too_many_arguments, reason = "pending_resume row columns")]
pub(super) fn upsert_row_with_input(
    store: &mut EventStore,
    session: &SessionId,
    turn_origin_seq: u64,
    interrupted_execution_id: &str,
    attempts: u32,
    state: ResumeState,
    reason: Option<&str>,
    updated_at: i64,
    active_input: Option<&MessageId>,
) -> Result<usize, String> {
    let aggregate = grapt_store::AggregateId::Session(session.clone());
    let (writer, elected_locally) = match store.fencing_token(&aggregate) {
        Ok(writer) => (writer, false),
        Err(grapt_store::WriteError::NotElected { .. }) => {
            store
                .elect_writer(&aggregate)
                .map_err(|error| format!("pending_resume writer election failed: {error}"))?;
            let writer = store
                .fencing_token(&aggregate)
                .map_err(|error| format!("pending_resume writer token failed: {error}"))?;
            (writer, true)
        }
        Err(error) => return Err(format!("pending_resume writer token failed: {error}")),
    };
    let result = (|| -> Result<usize, String> {
        let tx = store
            .governed_transaction()
            .map_err(|error| format!("pending_resume transaction refused: {error}"))?;
        let retired = upsert_row_within(
            &tx,
            writer,
            session,
            turn_origin_seq,
            interrupted_execution_id,
            attempts,
            state,
            reason,
            updated_at,
            active_input,
        )?;
        tx.commit()
            .map_err(|error| format!("pending_resume commit failed: {error}"))?
            .discard();
        Ok(retired)
    })();
    if elected_locally {
        store.release_held_writer(&aggregate);
    }
    result
}

pub(super) fn list_runnable_after(
    conn: &rusqlite::Connection,
    after: &str,
    limit: usize,
) -> Result<Vec<PendingRow>, String> {
    if !table_exists(conn, "pending_resume")? {
        return Ok(Vec::new());
    }
    let limit = i64::try_from(limit.max(1)).map_err(|error| error.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT session_id, turn_origin_seq, interrupted_execution_id, attempts
             FROM pending_resume
             WHERE state IN ('pending', 'running') AND session_id > ?1
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

struct AdmittedInputOrigin {
    seq: i64,
    input_type: Option<String>,
    user_origin: Option<String>,
}

fn admitted_input_by_id(
    conn: &rusqlite::Connection,
    session: &SessionId,
    input_id: &str,
) -> Result<Option<AdmittedInputOrigin>, String> {
    conn.query_row(
        "SELECT seq,
                CASE WHEN json_valid(data) THEN COALESCE(json_extract(data, '$.input.type'), json_extract(data, '$.kind'), 'user') END,
                CASE WHEN json_valid(data) THEN COALESCE(json_extract(data, '$.input.data.metadata.origin'), '') END
         FROM event
         WHERE aggregate_family = 'session' AND aggregate_id = ?1
           AND type = 'session.input.admitted.1'
           AND CASE WHEN json_valid(data)
               THEN json_extract(data, '$.inputID') = ?2
               ELSE 0
           END
         ORDER BY seq ASC
         LIMIT 1",
        rusqlite::params![session.as_str(), input_id],
        |row| {
            Ok(AdmittedInputOrigin {
                seq: row.get(0)?,
                input_type: row.get(1)?,
                user_origin: row.get(2)?,
            })
        },
    )
    .optional()
    .map_err(|error| format!("admitted input lookup failed: {error}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct InterruptedTurn {
    pub(super) origin_seq: u64,
    pub(super) execution_start_seq: u64,
}

/// Identify the durable human prompt `seq` that originated `execution` along
/// with `execution`'s own `session.execution.started.1` boundary.
///
/// Looks up the `session.input.promoted.1` event that opened `execution` (so
/// human prompts queued during a previous execution and promoted later retain
/// their true admission `seq`), and proves synthetic auto-resume lineage solely
/// through the Inbox-owned `session_resume_input` binding rather than
/// caller-writable metadata. Fails closed (`Ok(None)`) when `execution` has no
/// matching start, no promoted input, or was started by `seat-startup` or an
/// unrelated synthetic input.
pub(super) fn interrupted_turn(
    conn: &rusqlite::Connection,
    session: &SessionId,
    execution: &ExecutionId,
) -> Result<Option<InterruptedTurn>, String> {
    let Some(exec_start): Option<i64> = conn
        .query_row(
            "SELECT MIN(seq) FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type = 'session.execution.started.1'
               AND CASE WHEN json_valid(data)
                   THEN json_extract(data, '$.executionID') = ?2
                   ELSE 0
               END",
            rusqlite::params![session.as_str(), execution.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| format!("execution start lookup failed: {error}"))?
    else {
        return Ok(None);
    };
    let execution_start_seq =
        u64::try_from(exec_start).map_err(|_| "execution start seq negative".to_owned())?;

    let prev_terminal: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND seq < ?2
               AND type IN (
                   'session.execution.succeeded.1',
                   'session.execution.failed.1',
                   'session.execution.cancelled.1',
                   'session.execution.interrupted.1'
               )",
            rusqlite::params![session.as_str(), exec_start],
            |row| row.get(0),
        )
        .map_err(|error| format!("previous execution terminal lookup failed: {error}"))?;

    let promoted_input_id: Option<String> = conn
        .query_row(
            "SELECT CASE WHEN json_valid(data) THEN json_extract(data, '$.inputID') END
             FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type = 'session.input.promoted.1'
               AND seq > ?2 AND seq <= ?3
             ORDER BY seq DESC
             LIMIT 1",
            rusqlite::params![session.as_str(), prev_terminal, exec_start],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("promoted input lookup failed: {error}"))?
        .flatten();
    let Some(input_id) = promoted_input_id else {
        return Ok(None);
    };
    let Some(origin) = admitted_input_by_id(conn, session, &input_id)? else {
        return Ok(None);
    };
    let origin_seq = match origin.input_type.as_deref() {
        Some("user") if origin.user_origin.as_deref() != Some("seat-startup") => {
            Some(u64::try_from(origin.seq).map_err(|_| "turn origin seq negative".to_owned())?)
        }
        Some("synthetic") => grapt_inbox::Inbox::resume_input_origin(conn, session, &input_id)
            .map_err(|error| format!("resume input lineage lookup failed: {error}"))?,
        _ => None,
    };
    Ok(origin_seq.map(|origin_seq| InterruptedTurn {
        origin_seq,
        execution_start_seq,
    }))
}

/// Latest durable human prompt `seq` (excluding `seat-startup`) for detecting
/// whether a newer user prompt superseded a queued auto-resume.
pub(super) fn latest_human_input_seq(
    conn: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Option<u64>, String> {
    let seq: Option<i64> = conn
        .query_row(
            "SELECT MAX(seq) FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type = 'session.input.admitted.1'
               AND CASE WHEN json_valid(data)
                   THEN COALESCE(json_extract(data, '$.input.type'), json_extract(data, '$.kind'), 'user') = 'user'
                        AND COALESCE(json_extract(data, '$.input.data.metadata.origin'), '') != 'seat-startup'
                   ELSE 0
               END",
            [session.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| format!("human input lookup failed: {error}"))?;
    seq.map(|value| u64::try_from(value).map_err(|_| "human input seq negative".to_owned()))
        .transpose()
}

pub(super) fn is_child_session(
    conn: &rusqlite::Connection,
    session: &SessionId,
) -> Result<bool, String> {
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

/// Positive durable check that no provider attempt for `session` remains
/// unresolved in `attempt_receipt` or quarantined in `attempt_receipt_quarantine`.
pub(super) fn has_unresolved_or_quarantined_attempt(
    conn: &rusqlite::Connection,
    session: &SessionId,
) -> Result<bool, String> {
    for table in ["attempt_receipt", "attempt_receipt_quarantine"] {
        if !table_exists(conn, table)? {
            continue;
        }
        let sql = format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1");
        let count: i64 = conn
            .query_row(&sql, [session.as_str()], |row| row.get(0))
            .map_err(|error| format!("{table} lookup failed: {error}"))?;
        if count > 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

/// True if the latest `ProcessLoss` step failure since `since_seq` occurred
/// while a provider HTTP request could still have been in flight.
///
/// Only a durable terminal provider usage observation
/// (`session.usage.observed.2` with `$.usage.phase = 'terminal'`) in that step
/// proves `AttemptFragment::ProviderTerminal` arrived before process loss; a
/// streaming `ToolCalled` / `session.tool.%` row can commit while the provider
/// stream is still open and therefore never bypasses the Law D1 guard.
pub(super) fn has_ambiguous_process_loss_step(
    conn: &rusqlite::Connection,
    session: &SessionId,
    since_seq: u64,
) -> Result<bool, String> {
    let since = i64::try_from(since_seq).map_err(|_| "since_seq overflow".to_owned())?;
    let latest_fail_seq: Option<i64> = conn
        .query_row(
            "SELECT seq
             FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND seq > ?2
               AND type LIKE 'session.step.failed.%'
               AND CASE WHEN json_valid(data)
                   THEN json_extract(data, '$.reason.kind') = 'process-loss'
                   ELSE 0
               END
             ORDER BY seq DESC
             LIMIT 1",
            rusqlite::params![session.as_str(), since],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("process-loss step lookup failed: {error}"))?;
    let Some(fail_seq) = latest_fail_seq else {
        return Ok(false);
    };
    let step_floor: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(seq), ?2)
             FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND seq > ?2 AND seq < ?3
               AND (
                   type LIKE 'session.step.ended.%'
                   OR type LIKE 'session.step.failed.%'
                   OR type LIKE 'session.execution.started.%'
               )",
            rusqlite::params![session.as_str(), since, fail_seq],
            |row| row.get(0),
        )
        .map_err(|error| format!("step boundary lookup failed: {error}"))?;
    let terminal_provider_events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND seq > ?2 AND seq < ?3
               AND type = 'session.usage.observed.2'
               AND CASE WHEN json_valid(data)
                   THEN json_extract(data, '$.usage.phase') = 'terminal'
                   ELSE 0
               END",
            rusqlite::params![session.as_str(), step_floor, fail_seq],
            |row| row.get(0),
        )
        .map_err(|error| format!("step terminal usage lookup failed: {error}"))?;
    Ok(terminal_provider_events == 0)
}

/// Fail-closed check for open/suspended permissions or pending forms.
pub(super) fn has_pending_approval_or_form(
    conn: &rusqlite::Connection,
    session: &SessionId,
) -> Result<bool, String> {
    if table_exists(conn, "permission_request")? {
        let open_permissions: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM permission_request
                 WHERE session_id = ?1 AND status IN ('open', 'pending', 'suspended')",
                [session.as_str()],
                |row| row.get(0),
            )
            .map_err(|error| format!("permission_request lookup failed: {error}"))?;
        if open_permissions > 0 {
            return Ok(true);
        }
    }
    if table_exists(conn, "form_request")? {
        let open_forms: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM form_request
                 WHERE session_id = ?1 AND status = 'pending'",
                [session.as_str()],
                |row| row.get(0),
            )
            .map_err(|error| format!("form_request lookup failed: {error}"))?;
        if open_forms > 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Fail-closed check for active/lost subagent child operations owned by `session`.
pub(super) fn has_active_or_lost_subagent_child(
    conn: &rusqlite::Connection,
    session: &SessionId,
) -> Result<bool, String> {
    if !table_exists(conn, "subagent_operation")? {
        return Ok(false);
    }
    let active_children: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM subagent_operation
             WHERE parent_session_id = ?1 AND status IN ('admitted', 'running', 'ambiguous')",
            [session.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| format!("subagent_operation lookup failed: {error}"))?;
    Ok(active_children > 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SyntheticProgress {
    NotPromoted,
    PromotedNotStarted,
    Started,
}

/// Inspect durable progress for one synthetic resume `input`.
///
/// Only a `session.execution.started.1` committed after `input`'s own
/// `session.input.promoted.1` (and before any subsequent promotion) proves that
/// an execution actually started for this exact synthetic input. A crash
/// between promotion and execution start returns `PromotedNotStarted` so
/// restart can recover without stranding or falsely marking it `Completed`.
pub(super) fn synthetic_input_progress(
    conn: &rusqlite::Connection,
    session: &SessionId,
    input: &MessageId,
) -> Result<SyntheticProgress, String> {
    let promo_seq: Option<i64> = conn
        .query_row(
            "SELECT MIN(seq) FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type = 'session.input.promoted.1'
               AND CASE WHEN json_valid(data)
                   THEN json_extract(data, '$.inputID') = ?2
                   ELSE 0
               END",
            rusqlite::params![session.as_str(), input.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| format!("synthetic promotion lookup failed: {error}"))?;
    let Some(promo_seq) = promo_seq else {
        return Ok(SyntheticProgress::NotPromoted);
    };
    let next_promo_seq: i64 = conn
        .query_row(
            "SELECT MIN(seq) FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type = 'session.input.promoted.1'
               AND seq > ?2",
            rusqlite::params![session.as_str(), promo_seq],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(|error| format!("next promotion lookup failed: {error}"))?
        .unwrap_or(i64::MAX);
    let started: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type = 'session.execution.started.1'
               AND seq > ?2 AND seq < ?3
             LIMIT 1",
            rusqlite::params![session.as_str(), promo_seq, next_promo_seq],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("synthetic execution start lookup failed: {error}"))?;
    if started.is_some() {
        Ok(SyntheticProgress::Started)
    } else {
        Ok(SyntheticProgress::PromotedNotStarted)
    }
}

/// Collision-free database-wide deterministic `MessageId` derived via SHA-256
/// over `(session_id, turn_origin_seq, attempt)`.
pub(super) fn synthetic_message_id(
    session: &SessionId,
    turn_origin_seq: u64,
    attempt: u32,
) -> Result<MessageId, String> {
    let preimage = format!("{}\0{turn_origin_seq}\0{attempt}", session.as_str());
    let digest = ring::digest::digest(&ring::digest::SHA256, preimage.as_bytes());
    let mut hex = String::with_capacity(32);
    for byte in &digest.as_ref()[..16] {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    let raw = format!("msg_ar_{hex}");
    raw.parse::<MessageId>()
        .map_err(|error| format!("auto-resume message id invalid: {error}"))
}
