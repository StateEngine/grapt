//! Read-only unresolved-work guards. Historical rows are retained in the store,
//! but do not themselves prevent session tombstones. Never initialize owners here.
use std::collections::{BTreeMap, BTreeSet};

use grapt_types::SessionId;
use serde_json::Value;

use super::evidence::{protect_all, protect_session, required, SessionEvidence, Source};
use super::RetentionReason;

type Sessions = BTreeMap<SessionId, SessionEvidence>;

const REQUIRED: &[&str] = &[
    "attempt_receipt",
    "attempt_receipt_quarantine",
    "provider_attempt_accounting",
    "provider_attempt_coverage",
    "session_pending",
    "session_message",
    "inbox_legacy_synthetic_audit",
    "session_operation_delivery_pending",
    "session_resume_input",
    "permission_request",
    "form_request",
    "instruction_entry",
    "instruction_state",
    "subagent_operation",
    "subagent_followup",
    "subagent_run",
    "shell_operation",
    "mailbox_message",
    "session_list_projection",
];

const OPTIONAL_KNOWN: &[&str] = &[
    "event",
    "event_sequence",
    "aggregate_writer",
    "session",
    "session_list_legacy_baseline",
    "product_shell_binding",
    "permission_http_request",
    "permission_http_rule",
    "subagent_operation_legacy_synthetic",
    "subagent_operation_pre_delivery_settlement",
    "session_operation_delivery_completed_audit",
    "compaction_window_cache",
    "compaction_cache_watermark",
    "plan_read_receipt",
    "pending_resume",
];

fn validate_schema_tables(source: &Source<'_>, tables: &BTreeSet<String>) -> Result<(), String> {
    for table in REQUIRED {
        if !tables.contains(*table) {
            return Err(format!(
                "required evidence table {table} is absent; cleanup does not repair schemas"
            ));
        }
    }
    for table in tables {
        if REQUIRED.contains(&table.as_str()) || OPTIONAL_KNOWN.contains(&table.as_str()) {
            continue;
        }
        if source
            .columns(table)?
            .iter()
            .any(|column| column.to_ascii_lowercase().contains("session"))
        {
            return Err(format!("unfamiliar session-bearing evidence table {table}"));
        }
    }
    Ok(())
}

pub(super) fn protect(
    source: &Source<'_>,
    sessions: &mut Sessions,
    cutoff: i64,
) -> Result<(), String> {
    let mut tables = BTreeSet::new();
    source.select(
        "SELECT CASE WHEN length(CAST(name AS BLOB))<=128 THEN name END
        FROM sqlite_master WHERE type IN ('table','view') LIMIT 129",
        |[name]| {
            let name = required(name)?;
            if !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                return Err("unfamiliar SQL object name prevents evidence inspection".into());
            }
            tables.insert(name);
            if tables.len() > 128 {
                return Err("cleanup schema budget exceeded".into());
            }
            Ok(())
        },
    )?;
    validate_schema_tables(source, &tables)?;
    // Pending means unpromoted work, not the permanent session_message history.
    // These predicates include unknown/NULL states rather than silently hiding them.
    for query in [
        "SELECT CASE WHEN length(CAST(session_id AS BLOB))<=512 THEN session_id END FROM session_pending LIMIT 100001",
        // The inbox owner keeps these migrated bytes permanently. Their
        // sessions remain unsupported legacy histories, not cleanup candidates.
        "SELECT CASE WHEN length(CAST(session_id AS BLOB))<=512 THEN session_id END FROM inbox_legacy_synthetic_audit LIMIT 100001",
        "SELECT CASE WHEN length(CAST(session_id AS BLOB))<=512 THEN session_id END FROM session_operation_delivery_pending LIMIT 100001",
        "SELECT CASE WHEN length(CAST(session_id AS BLOB))<=512 THEN session_id END FROM session_resume_input WHERE active IS NULL OR active<>0 LIMIT 100001",
        "SELECT CASE WHEN length(CAST(session_id AS BLOB))<=512 THEN session_id END FROM attempt_receipt_quarantine LIMIT 100001",
        "SELECT CASE WHEN length(CAST(session_id AS BLOB))<=512 THEN session_id END FROM permission_request WHERE status IS NULL OR status NOT IN ('replied','cancelled') LIMIT 100001",
        "SELECT CASE WHEN length(CAST(session_id AS BLOB))<=512 THEN session_id END FROM form_request WHERE session_id IS NOT NULL AND (status IS NULL OR status NOT IN ('answered','cancelled')) LIMIT 100001",
        "SELECT CASE WHEN length(CAST(session_id AS BLOB))<=512 THEN session_id END FROM provider_attempt_coverage WHERE complete IS NULL OR complete<>1 LIMIT 100001",
        "SELECT CASE WHEN length(CAST(r.child_session_id AS BLOB))<=512 THEN r.child_session_id END FROM subagent_run r WHERE NOT EXISTS(SELECT 1 FROM subagent_operation o WHERE o.operation_id=r.operation_id AND o.child_session_id=r.child_session_id) LIMIT 100001",
    ] {
        source.select(query, |[session]| protect_session(sessions, required(session)?))?;
    }
    // HTTP rows are adjunct history; the authoritative request status above owns
    // pendingness. An orphan adjunct has lost its Session attribution and refuses.
    if tables.contains("pending_resume") {
        source.select(
            "SELECT CASE WHEN length(CAST(session_id AS BLOB))<=512 THEN session_id END
             FROM pending_resume
             WHERE state IS NULL OR state NOT IN ('completed','stopped')
             LIMIT 100001",
            |[session]| protect_session(sessions, required(session)?),
        )?;
    }
    if tables.contains("permission_http_request") {
        source.select(
            "SELECT 'orphan' FROM permission_http_request h WHERE NOT EXISTS(
            SELECT 1 FROM permission_request p WHERE p.id=h.id) LIMIT 1",
            |[_]| Err("permission HTTP evidence has no owning request".into()),
        )?;
    }
    lineage(source, sessions, &tables)?;
    source.select("SELECT CASE WHEN length(CAST(f.sender_session_id AS BLOB))<=512 THEN f.sender_session_id END,
        CASE WHEN length(CAST(f.child_session_id AS BLOB))<=512 THEN f.child_session_id END
        FROM subagent_followup f WHERE f.state IS NULL OR f.state NOT IN ('consumed','cancelled','started')
          OR (f.state='started' AND NOT EXISTS(SELECT 1 FROM subagent_operation o
            WHERE o.operation_id=f.operation_id AND o.child_session_id=f.child_session_id
              AND o.status IN ('completed','failed','cancelled')
              AND (o.mode='foreground' OR (o.mode='background' AND o.delivery='delivered'
                AND o.time_acknowledged IS NOT NULL AND o.time_cleaned IS NOT NULL)))) LIMIT 100001",
        |[sender, child]| {
            protect_session(sessions, required(sender)?)?;
            protect_session(sessions, required(child)?)
        })?;
    super::receipts::protect(source, sessions, cutoff)?;
    shells(
        source,
        sessions,
        tables.contains("product_shell_binding"),
        cutoff,
    )?;
    mailbox(source, sessions)
}

fn family(sessions: &mut Sessions, raw: String) -> Result<(), String> {
    let session: SessionId = raw
        .parse()
        .map_err(|error| format!("invalid lineage identity: {error}"))?;
    if let Some(state) = sessions.get_mut(&session) {
        state.retain(RetentionReason::Family);
    }
    Ok(())
}

fn lineage(
    source: &Source<'_>,
    sessions: &mut Sessions,
    tables: &BTreeSet<String>,
) -> Result<(), String> {
    if tables.contains("session_list_legacy_baseline") {
        // A baseline carries identity/age but not parent identity. Never infer
        // leafhood from a missing/tombstoned live-list row.
        let query = if tables.contains("session") {
            "SELECT 'missing' FROM session_list_legacy_baseline b
             WHERE NOT EXISTS(SELECT 1 FROM session s WHERE s.id=b.session_id) LIMIT 1"
        } else {
            "SELECT 'missing' FROM session_list_legacy_baseline LIMIT 1"
        };
        source.select(query, |[_]| {
            Err("legacy baseline lacks its lineage source".into())
        })?;
    }
    // Operation existence proves a child relation even after settlement. Do not
    // rely on the live session list or discard tombstoned descendants.
    for table in [
        "subagent_operation",
        "subagent_operation_legacy_synthetic",
        "subagent_operation_pre_delivery_settlement",
    ] {
        if tables.contains(table) {
            source.select(&format!("SELECT CASE WHEN length(CAST(parent_session_id AS BLOB))<=512 THEN parent_session_id END,
                CASE WHEN length(CAST(child_session_id AS BLOB))<=512 THEN child_session_id END FROM {table} LIMIT 100001"),
                |[parent, child]| {
                    family(sessions, required(parent)?)?;
                    family(sessions, required(child)?)
                })?;
        }
    }
    for (table, child) in [("session_list_projection", "session_id"), ("session", "id")] {
        if tables.contains(table) {
            source.select(
                &format!(
                    "SELECT CASE WHEN length(CAST({child} AS BLOB))<=512 THEN {child} END,
                CASE WHEN length(CAST(parent_id AS BLOB))<=512 THEN parent_id END
                FROM {table} WHERE parent_id IS NOT NULL LIMIT 100001"
                ),
                |[child, parent]| {
                    family(sessions, required(child)?)?;
                    family(sessions, required(parent)?)
                },
            )?;
        }
    }
    Ok(())
}

fn shells(
    source: &Source<'_>,
    sessions: &mut Sessions,
    has_binding: bool,
    cutoff: i64,
) -> Result<(), String> {
    // Process exit, pipe drain and cleanup are independent owner facts.
    // Only unresolved operations need attribution; settled bindings are history.
    let (metadata, join) = if has_binding {
        (
            "CASE WHEN length(CAST(b.metadata AS BLOB))<=16384 THEN b.metadata END",
            " LEFT JOIN product_shell_binding b ON b.operation_id=o.operation_id",
        )
    } else {
        ("NULL", "")
    };
    source.select(
        &format!(
            "SELECT {metadata} FROM shell_operation o{join}
        WHERE o.status IS NULL OR o.status NOT IN ('exited','timed-out','cancelled','failed')
          OR o.pipe IS NULL OR o.pipe NOT IN ('drained','timed-out')
          OR o.cleanup IS NULL OR o.cleanup<>'complete'
          OR o.time_updated IS NULL OR o.time_updated<0 OR o.time_updated>={cutoff}
          OR o.time_created IS NULL OR o.time_created<0 OR o.time_created>={cutoff} LIMIT 100001"
        ),
        |[metadata]| {
            let session = metadata
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .and_then(|value| {
                    value
                        .get("sessionID")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .and_then(|raw| raw.parse::<SessionId>().ok());
            if let Some(session) = session {
                protect_session(sessions, session.as_str().to_owned())?;
            } else {
                protect_all(sessions);
            }
            Ok(())
        },
    )
}

fn mailbox(source: &Source<'_>, sessions: &mut Sessions) -> Result<(), String> {
    // Safe limitation: consumed/acknowledged rows can still owe metadata repair
    // (mailbox/remote.rs::RemoteMailboxRecord::repair_pending). That state lives
    // inside a private binary codec with no read-only owner seam available here.
    // Until it is exposed, attribute EVERY retained row, not just indexed debt;
    // an empty mailbox has no guard, and mapped records affect only their sessions.
    let mut recipients = BTreeSet::new();
    source.select(
        "SELECT CASE WHEN length(CAST(recipient AS BLOB))<=512 THEN recipient END
        FROM mailbox_message LIMIT 100001",
        |[recipient]| {
            recipients.insert(required(recipient)?);
            Ok(())
        },
    )?;
    if recipients.is_empty() {
        return Ok(());
    }
    let mut bindings: BTreeMap<String, BTreeSet<SessionId>> = BTreeMap::new();
    source.select(
        "SELECT CASE WHEN length(CAST(session_id AS BLOB))<=512 THEN session_id END,
        CASE WHEN length(CAST(value AS BLOB))<=65536 THEN value END FROM instruction_entry
        WHERE key='stateengine-identity' AND removed=0 LIMIT 100001",
        |[session, raw]| {
            let session: SessionId = required(session)?
                .parse()
                .map_err(|error| format!("invalid mail session: {error}"))?;
            let value: Value = serde_json::from_str(&required(raw)?)
                .map_err(|_| "unreadable native mail binding")?;
            if value.get("scope").and_then(Value::as_str) == Some("incognito") {
                return Ok(());
            }
            let agent = value
                .get("agent_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or("native mail binding lacks recipient")?;
            if value
                .get("project_id")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            {
                return Err("native mail binding lacks project".into());
            }
            bindings
                .entry(agent.to_owned())
                .or_default()
                .insert(session);
            Ok(())
        },
    )?;
    for recipient in recipients {
        let mut mapped = false;
        if let Ok(session) = recipient.parse::<SessionId>() {
            if sessions.contains_key(&session) {
                protect_session(sessions, session.as_str().to_owned())?;
                mapped = true;
            }
        }
        if let Some(targets) = bindings.get(&recipient) {
            mapped = true;
            for session in targets {
                protect_session(sessions, session.as_str().to_owned())?;
            }
        }
        if !mapped {
            protect_all(sessions);
        }
    }
    Ok(())
}
