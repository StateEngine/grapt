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
        .prepare("SELECT DISTINCT session_id FROM form_request WHERE status = 'pending'")
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
) -> Result<(), String> {
    let seq = i64::try_from(turn_origin_seq)
        .map_err(|_| "turn_origin_seq overflows SQLite INTEGER".to_owned())?;
    let attempts = i64::from(attempts);
    let tx = store
        .governed_transaction()
        .map_err(|error| format!("pending_resume transaction refused: {error}"))?;
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
    tx.commit()
        .map_err(|error| format!("pending_resume commit failed: {error}"))?
        .discard();
    Ok(())
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

/// Identify the durable human prompt `seq` that originated `execution`,
/// excluding `seat-startup` inputs and unpromoted queued prompts admitted after
/// `execution` started.
pub(super) fn interrupted_turn_origin_seq(
    conn: &rusqlite::Connection,
    session: &SessionId,
    execution: &ExecutionId,
) -> Result<Option<u64>, String> {
    let exec_start: Option<i64> = conn
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
        .map_err(|error| format!("execution start lookup failed: {error}"))?;

    let has_pending_table = table_exists(conn, "session_pending")?;
    let sql = match (exec_start.is_some(), has_pending_table) {
        (true, true) => {
            "SELECT MAX(seq) FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type = 'session.input.admitted.1'
               AND seq <= ?2
               AND CASE WHEN json_valid(data)
                   THEN COALESCE(json_extract(data, '$.input.type'), json_extract(data, '$.kind'), 'user') = 'user'
                        AND COALESCE(json_extract(data, '$.input.data.metadata.origin'), '') != 'seat-startup'
                        AND COALESCE(json_extract(data, '$.inputID'), '') NOT IN (
                            SELECT id FROM session_pending WHERE session_id = ?1
                        )
                   ELSE 0
               END"
        }
        (true, false) => {
            "SELECT MAX(seq) FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type = 'session.input.admitted.1'
               AND seq <= ?2
               AND CASE WHEN json_valid(data)
                   THEN COALESCE(json_extract(data, '$.input.type'), json_extract(data, '$.kind'), 'user') = 'user'
                        AND COALESCE(json_extract(data, '$.input.data.metadata.origin'), '') != 'seat-startup'
                   ELSE 0
               END"
        }
        (false, true) => {
            "SELECT MAX(seq) FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type = 'session.input.admitted.1'
               AND CASE WHEN json_valid(data)
                   THEN COALESCE(json_extract(data, '$.input.type'), json_extract(data, '$.kind'), 'user') = 'user'
                        AND COALESCE(json_extract(data, '$.input.data.metadata.origin'), '') != 'seat-startup'
                        AND COALESCE(json_extract(data, '$.inputID'), '') NOT IN (
                            SELECT id FROM session_pending WHERE session_id = ?1
                        )
                   ELSE 0
               END"
        }
        (false, false) => {
            "SELECT MAX(seq) FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type = 'session.input.admitted.1'
               AND CASE WHEN json_valid(data)
                   THEN COALESCE(json_extract(data, '$.input.type'), json_extract(data, '$.kind'), 'user') = 'user'
                        AND COALESCE(json_extract(data, '$.input.data.metadata.origin'), '') != 'seat-startup'
                   ELSE 0
               END"
        }
    };
    let seq: Option<i64> = if let Some(bound) = exec_start {
        conn.query_row(sql, rusqlite::params![session.as_str(), bound], |row| {
            row.get(0)
        })
    } else {
        conn.query_row(sql, [session.as_str()], |row| row.get(0))
    }
    .map_err(|error| format!("interrupted turn origin lookup failed: {error}"))?;
    seq.map(|value| u64::try_from(value).map_err(|_| "turn origin seq negative".to_owned()))
        .transpose()
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
/// while a provider HTTP request was still in flight (no `session.tool.%`
/// event committed in that step). When a tool call already started/settled
/// inside that step, the provider response had already arrived and the turn is
/// safe to auto-resume once lost tool runs are settled.
pub(super) fn has_ambiguous_process_loss_step(
    conn: &rusqlite::Connection,
    session: &SessionId,
    since_seq: u64,
) -> Result<bool, String> {
    let since = i64::try_from(since_seq).map_err(|_| "since_seq overflow".to_owned())?;
    let latest_failure: Option<(i64, Option<i64>)> = conn
        .query_row(
            "SELECT seq, CASE WHEN json_valid(data) THEN json_extract(data, '$.step') END
             FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND seq > ?2
               AND type = 'session.step.failed.2'
               AND CASE WHEN json_valid(data)
                   THEN json_extract(data, '$.reason') = 'process_loss'
                        OR json_extract(data, '$.reason.type') = 'process_loss'
                   ELSE 0
               END
             ORDER BY seq DESC
             LIMIT 1",
            rusqlite::params![session.as_str(), since],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| format!("process-loss step lookup failed: {error}"))?;
    let Some((fail_seq, Some(step))) = latest_failure else {
        return Ok(latest_failure.is_some());
    };
    let tool_events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND seq > ?2 AND seq < ?3
               AND type LIKE 'session.tool.%'
               AND CASE WHEN json_valid(data)
                   THEN json_extract(data, '$.step') = ?4
                   ELSE 0
               END",
            rusqlite::params![session.as_str(), since, fail_seq, step],
            |row| row.get(0),
        )
        .map_err(|error| format!("step tool event lookup failed: {error}"))?;
    Ok(tool_events == 0)
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

pub(super) fn synthetic_input_admitted(
    conn: &rusqlite::Connection,
    session: &SessionId,
    input: &MessageId,
) -> Result<bool, String> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type = 'session.input.admitted.1'
               AND CASE WHEN json_valid(data)
                   THEN json_extract(data, '$.inputID') = ?2
                   ELSE 0
               END
             LIMIT 1",
            rusqlite::params![session.as_str(), input.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("synthetic admission lookup failed: {error}"))?;
    Ok(found.is_some())
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
