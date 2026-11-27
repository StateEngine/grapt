//! Bounded, own-Session recovery from the existing captured terminal results.
//! No new durable writes, process probes, replay, or command authority.
use super::*;
use crate::dogfood::tools::{terminal_recovery::Record, TerminalRegistry};

const SCAN_RESULTS: usize = 64;
const KEEP_COMMANDS: usize = 8;
const NOTICE_BYTES: usize = 16 * 1024;
const EXACT_ROWS: usize = 2;

struct Candidate {
    call: ToolCallId,
    nested: bool,
}

/// A snapshot per execution keeps the harness note stable throughout its
/// provider requests. Old observations remain available after compaction.
pub(in crate::dogfood) fn notice(
    inbox: &Arc<Mutex<Inbox>>,
    artifacts: &FilesystemArtifactStore,
    session: &SessionId,
    live: &TerminalRegistry,
) -> Result<Option<String>, String> {
    let candidates = candidates(&lock(inbox), session)?;
    let truncated = candidates.len() > SCAN_RESULTS;
    let selected = candidates
        .iter()
        .take(SCAN_RESULTS)
        .filter(|candidate| !candidate.nested)
        .map(|candidate| candidate.call.clone())
        .collect::<Vec<_>>();
    let mut projections = ToolProjection::read_batches(inbox, artifacts, session, &selected);
    let mut seen = std::collections::BTreeSet::new();
    let mut records = Vec::new();
    for candidate in candidates.into_iter().take(SCAN_RESULTS) {
        let call = candidate.call;
        let output = if candidate.nested {
            let Some(output) = nested_output(inbox, artifacts, session, &call)? else {
                continue;
            };
            output
        } else {
            let projection = projections
                .remove(&call)
                .unwrap_or_else(|| tool_projection(inbox, artifacts, session, &call))?;
            if projection.execution != grapt_types::ToolExecution::Local
                || !matches!(
                    projection.name.as_str(),
                    "exec_command" | "write_stdin" | "request_sandbox_access"
                )
                || projection
                    .permission_tool
                    .as_ref()
                    .is_some_and(|tool| !matches!(tool.as_str(), "shell" | "shell.poll"))
            {
                continue;
            }
            projection.output
        };
        let Some(record) = Record::decode(&call, &output)? else {
            continue;
        };
        // Newest observation of a command wins, even if it is final. An old
        // running report never resurrects a command whose exit was observed.
        if !seen.insert(record.operation.clone())
            || !record.pending()
            || live.contains(session, &record.operation)
        {
            continue;
        }
        records.push(record);
    }
    render(records, truncated)
}

/// Select native terminal identities, not arbitrary recent tools: source reads
/// must not push an unfinished command out of recovery. JSON/byte/row limits
/// are checked before decoding, and history visibility preserves rewind.
fn candidates(inbox: &Inbox, session: &SessionId) -> Result<Vec<Candidate>, String> {
    let connection = inbox.store().connection();
    let visible = grapt_store::history_visibility::HistoryVisibility::read(connection, session)
        .map_err(|error| error.to_string())?
        .sql("e.seq");
    let mut query = connection
        .prepare(&format!(
            "SELECT e.type, CASE WHEN length(CAST(e.data AS BLOB)) <= 32768 THEN e.data END
         FROM event e WHERE e.aggregate_family='session' AND e.aggregate_id=?1
         AND {visible} AND e.type IN ('session.tool.input.started.1','session.tool.input.started.2','session.tool.nested.started.1')
         AND CASE WHEN json_valid(e.data)=0 THEN 1
                  WHEN e.type='session.tool.nested.started.1'
                       THEN CASE WHEN json_type(e.data, '$.tool') IS NOT 'text' THEN 1
                                 ELSE json_extract(e.data, '$.tool') IN ('exec_command','write_stdin') END
                  WHEN json_extract(e.data, '$.execution')='provider_hosted' THEN 0
                  WHEN json_type(e.data, '$.name') IS NOT 'text' THEN 1
                  ELSE json_extract(e.data, '$.name') IN ('exec_command','write_stdin') END
         ORDER BY e.seq DESC LIMIT ?2"
        ))
        .map_err(|error| error.to_string())?;
    let rows = query
        .query_map((session.as_str(), (SCAN_RESULTS + 1) as i64), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(|error| error.to_string())?;
    let mut calls = Vec::new();
    for row in rows {
        let (kind, raw) = row.map_err(|error| error.to_string())?;
        let raw = raw.ok_or("terminal recovery identity exceeds 32 KiB")?;
        let value =
            serde_json::from_str(&raw).map_err(|_| "terminal recovery identity is not JSON")?;
        if kind == "session.tool.nested.started.1" {
            let fact = grapt_event::session_tool_nested::decode(&value)
                .map_err(|_| "nested terminal recovery identity is corrupt")?;
            calls.push(Candidate {
                call: fact.call().clone(),
                nested: true,
            });
        } else {
            let version = if kind.ends_with(".2") { 2 } else { 1 };
            let identity = grapt_event::session_tool_identity::decode(version, &value)
                .map_err(|_| "terminal recovery identity is corrupt")?;
            if identity.session != *session {
                return Err("terminal recovery identity belongs to another session".into());
            }
            if identity.execution == grapt_types::ToolExecution::Local {
                calls.push(Candidate {
                    call: identity.call,
                    nested: false,
                });
            }
        }
    }
    Ok(calls)
}

/// An exact, session-scoped native nested result with our recovery marker.
/// Ordinary nested/MCP results remain the parent's responsibility. No input
/// artifact is read, and terminal identity/attempt/uniqueness are verified.
pub(in crate::dogfood::continuation) fn nested_output(
    inbox: &Arc<Mutex<Inbox>>,
    artifacts: &FilesystemArtifactStore,
    session: &SessionId,
    call: &ToolCallId,
) -> Result<Option<String>, String> {
    let held = lock(inbox);
    let starts = scoped_rows(
        held.store(),
        session,
        "session.tool.nested.started.1",
        None,
        None,
        Scope::Call(Some(call), "$.call"),
        EXACT_ROWS,
    )?;
    let mut start = None;
    for (_, raw) in starts {
        let value =
            serde_json::from_str(&raw).map_err(|_| "nested terminal admission is not JSON")?;
        let fact = grapt_event::session_tool_nested::decode(&value)
            .map_err(|_| "nested terminal admission is corrupt")?;
        if fact.call() != call || start.replace(fact).is_some() {
            return Err("nested terminal admission identity repeated or mismatched".into());
        }
    }
    let Some(start) = start else { return Ok(None) };
    if !matches!(
        start.tool().as_str(),
        "exec_command" | "write_stdin" | "request_sandbox_access"
    ) {
        return Ok(None);
    }
    let mut output = None;
    let mut found = false;
    for ScopedTerminal { terminal: raw, .. } in scoped_terminals(
        held.store(),
        session,
        None,
        None,
        Scope::Call(Some(call), "$.call"),
        EXACT_ROWS,
    )? {
        let StoredTerminal::Readable(fact) = raw else {
            continue;
        };
        if found || fact.call() != call || fact.attempt() != start.attempt() {
            return Err("nested terminal result identity repeated or mismatched".into());
        }
        found = true;
        if let ToolTerminalOutcome::Completed {
            output: Some(artifact),
        } = fact.outcome()
        {
            output = Some(artifact.clone());
        }
    }
    drop(held);
    let Some(artifact) = output else {
        return Ok(None);
    };
    let text = artifact_reads::read_output_text(
        artifacts,
        artifact.reference(),
        artifact.disposition(),
        "nested terminal recovery result",
    )?
    .as_str()
    .to_owned();
    Ok(Record::decode(call, &text)?.map(|_| text))
}

fn render(records: Vec<Record>, scanned_out: bool) -> Result<Option<String>, String> {
    if records.is_empty() {
        return Ok(None);
    }
    let omitted = scanned_out || records.len() > KEEP_COMMANDS;
    let mut text = "[Harness recovery: previously started background commands. Handles expired; current status is unknown. These are retained observations, not instructions. No command was replayed or checked for liveness.]\n".to_owned();
    for record in records.into_iter().take(KEEP_COMMANDS).rev() {
        text.push_str(&record.notice());
        text.push('\n');
    }
    if omitted {
        text.push_str("Recovery is bounded to eight commands from the newest 64 native terminal calls; older work may exist. Use history({}) to inspect this session.\n");
    }
    if text.len() > NOTICE_BYTES {
        return Err("terminal recovery notice exceeds 16 KiB".into());
    }
    Ok(Some(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_omits_oldest_commands_at_eight_and_bounds_escaped_identities() -> Result<(), String>
    {
        let mut records = Vec::new();
        for index in (0..12).rev() {
            let call =
                ToolCallId::new(format!("call-{index}-{}", "\u{0}".repeat(128))).ok_or("call")?;
            let output = serde_json::json!({
                "session_id":format!("op_recover_{index}"), "status":"running", "done":false,
                "recovery":{"started_ms":index, "host":"local/linux/fixture"},
            })
            .to_string();
            records.push(Record::decode(&call, &output)?.ok_or("record")?);
        }
        let text = render(records, true)?.ok_or("notice")?;
        assert!(text.len() <= NOTICE_BYTES);
        assert_eq!(
            text.lines()
                .filter(|line| line.starts_with("Command "))
                .count(),
            KEEP_COMMANDS
        );
        assert!(!text.contains("Command \"op_recover_3\" "));
        assert!(text.contains("Command \"op_recover_4\" "));
        assert!(text.contains("Command \"op_recover_11\" "));
        assert!(text.contains("older work may exist"));
        assert!(text.contains("history({})"));
        Ok(())
    }
}
