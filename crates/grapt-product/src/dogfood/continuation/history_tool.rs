use super::*;
mod decoded;
mod page;
mod source;
pub(in crate::dogfood) use source::history;
#[cfg(test)]
mod skill_failure_tests;
pub(super) use decoded::{
    merge_identities, permission_calls as decode_permission_calls,
    tool_identities as decode_tool_identities,
};

fn tool_matches_permission(tool: &ToolName, action: &ToolName) -> bool {
    tool == action
        || matches!(
            (tool.as_str(), action.as_str()),
            ("read_history", "history")
                | ("view_image", "read")
                | ("apply_patch", "patch")
                | ("exec_command", "shell")
                | ("request_sandbox_access", "sandbox.write")
                | (
                    "exec_command" | "shell",
                    grapt_permission::SANDBOX_MSYS_ACTION
                )
                | ("write_stdin", "shell" | "shell.poll")
        )
}

/// An unknown effect no longer quarantines the Session, so its model-visible
/// line carries the warning that replaces the quarantine.
const UNKNOWN_EFFECT_NOTICE: &str = "Tool failed: EffectUnknown. Its effect state is unknown: it may have partly applied or may still be running. Verify the current state before continuing; do not repeat it blindly.";

pub(super) fn failure_line(reason: grapt_types::ToolFailureReason) -> String {
    match reason {
        grapt_types::ToolFailureReason::EffectUnknown => UNKNOWN_EFFECT_NOTICE.to_owned(),
        reason => format!("Tool failed: {reason:?}"),
    }
}

pub(in crate::dogfood) fn tool_projection(
    inbox: &Arc<Mutex<Inbox>>,
    artifacts: &FilesystemArtifactStore,
    session: &SessionId,
    target: &ToolCallId,
) -> Result<ToolProjection, String> {
    let inbox = lock(inbox);
    project_tool(inbox.store(), artifacts, session, target)
}

#[derive(Debug)]
struct RawStoredOutput {
    seq: u64,
    call: ToolCallId,
    output: String,
    error: Option<String>,
    failure_reason: Option<grapt_types::ToolFailureReason>,
    capture_failed: bool,
}

fn project_tool(
    store: &EventStore,
    artifacts: &FilesystemArtifactStore,
    session: &SessionId,
    target: &ToolCallId,
) -> Result<ToolProjection, String> {
    let window = BoundedWindow {
        from: None,
        until: None,
        max_rows: MAX_CONTINUATION_ROWS,
    };
    let calls = stored_calls_for(store, session, artifacts, window, Some(target), None)?;
    let rows = scoped_terminals(
        store,
        session,
        window.from,
        window.until,
        Scope::Model(Some(target)),
        window.max_rows,
    )?
    .into_iter()
    .map(|row| (row.seq, row.terminal))
    .collect();
    let outputs = assemble_raw_outputs(rows, artifacts, None, None, false)?;

    finish_projection(store, artifacts, session, target, calls, outputs)
}

fn finish_projection(
    store: &EventStore,
    artifacts: &FilesystemArtifactStore,
    session: &SessionId,
    target: &ToolCallId,
    calls: Vec<StoredCall>,
    outputs: Vec<RawStoredOutput>,
) -> Result<ToolProjection, String> {
    let mut matching_calls = calls.iter().filter(|entry| entry.call == *target);
    let call = matching_calls
        .next()
        .ok_or_else(|| format!("tool {target} has no durable ToolCalled content"))?;
    if matching_calls.next().is_some() {
        return Err(format!(
            "tool {target} has duplicate durable ToolCalled content"
        ));
    }
    let mut matching_outputs = outputs.iter().filter(|entry| entry.call == *target);
    let output = matching_outputs.next();
    if matching_outputs.next().is_some() {
        return Err(format!(
            "tool {target} has duplicate durable terminal outputs"
        ));
    }
    // A receipt that does not parse is a lost output, as one failing its
    // digest is, so every history view shows the same failed outcome.
    let corrupt_image = output.and_then(|output| corrupt_image_receipt(call, output));
    if let Some(error) = &corrupt_image {
        crate::diagnostics::report(format!(
            "native image receipt of {target} is corrupt: {error}"
        ));
    }
    // Empty arguments mean {} (as the provider sink reads them); any other
    // malformed stored call is reported, never a reason to fail the history
    // read, and with it the turn.
    let arguments = call.arguments.as_str();
    let (input, input_invalid) = if arguments.trim().is_empty() {
        (serde_json::json!({}), None)
    } else {
        match serde_json::from_str::<serde_json::Value>(arguments) {
            Ok(input) if input.is_object() => (input, None),
            Ok(_) => (
                serde_json::json!({}),
                Some("arguments are not a JSON object".to_owned()),
            ),
            Err(error) => (
                serde_json::json!({}),
                Some(format!("arguments are not JSON: {error}")),
            ),
        }
    };
    Ok(ToolProjection {
        execution: call.execution,
        name: call
            .tool
            .as_ref()
            .map_or("unavailable tool", ToolName::as_str)
            .to_owned(),
        permission_tool: call.permission_tool.clone(),
        image_read: call.image_read,
        input,
        input_invalid,
        output: match corrupt_image {
            Some(_) => super::artifact_reads::unreadable_output("image output"),
            None => output.map_or_else(String::new, |output| output.output.as_str().to_owned()),
        },
        error: match corrupt_image {
            Some(_) => Some(UNREADABLE.to_owned()),
            None => output.and_then(|output| output.error.clone()),
        },
        failure_reason: output.and_then(|output| output.failure_reason),
        settled: output.is_some(),
        never_ran: output.is_none() && effect_withheld(store, session, target)?,
        progress: progress::snapshot(store, session, target, false)?,
        files: if output.is_some_and(|output| !output.capture_failed) {
            files::collect(store, artifacts, session, target)?
        } else {
            Vec::new()
        },
    })
}

/// Why a settled local image read's receipt does not parse, when it does not.
fn corrupt_image_receipt(call: &StoredCall, output: &RawStoredOutput) -> Option<String> {
    use crate::dogfood::tools::image;
    let receipt = output.output.as_str();
    let image_read = call.execution == grapt_types::ToolExecution::Local
        && call
            .tool
            .as_ref()
            .is_some_and(|tool| tool.as_str() == image::TOOL)
        && call.permission_tool.as_ref().map_or_else(
            || call.image_read.is_some() || image::has_receipt(receipt),
            |tool| tool.as_str() == "read",
        );
    (image_read && output.error.is_none())
        .then(|| image::replay(receipt).err())
        .flatten()
}

pub(super) fn stored_calls(
    store: &EventStore,
    session: &SessionId,
    artifacts: &FilesystemArtifactStore,
    from: Option<grapt_types::EventSeq>,
    until: Option<grapt_types::EventSeq>,
) -> Result<Vec<StoredCall>, String> {
    stored_calls_for(
        store,
        session,
        artifacts,
        BoundedWindow {
            from,
            until,
            max_rows: MAX_CONTINUATION_ROWS,
        },
        None,
        None,
    )
    .map_err(String::from)
}

pub(super) fn stored_calls_for(
    store: &EventStore,
    session: &SessionId,
    artifacts: &FilesystemArtifactStore,
    window: BoundedWindow,
    target: Option<&ToolCallId>,
    budget: Option<&mut usize>,
) -> Result<Vec<StoredCall>, recent::WindowError> {
    let history = SessionHistoryStore::over(store);
    let page = match (target, window.from, window.until) {
        (Some(target), _, _) => history.tool_call_page_for(session, target),
        (None, Some(from), Some(until)) => {
            if window.max_rows == grapt_session::MAX_TOOL_CALL_PAGE_ROWS {
                history.tool_call_page_window_from(session, from, until)
            } else {
                history.tool_call_page_window_from_bounded(
                    session,
                    from,
                    until,
                    window.max_rows,
                    MAX_CONTEXT_BYTES,
                )
            }
        }
        (None, Some(from), None) => history.tool_call_page_from(session, from),
        (None, None, Some(until)) => history.tool_call_page_window(session, until),
        (None, None, None) => history.tool_call_page(session),
    }
    .map_err(|error| {
        let text = format!("tool continuation history is unreadable: {error}");
        match error {
            grapt_session::HistoryReadError::WindowDataTooLong { .. } => {
                recent::WindowError::Capacity(text)
            }
            _ => recent::WindowError::Failed(text),
        }
    })?;
    let permissions = permission_calls_for(
        store,
        session,
        window.from,
        window.until,
        target,
        window.max_rows,
    )?;
    let identities = tool_identities(
        store,
        session,
        window.from,
        window.until,
        target,
        window.max_rows,
    )?;
    assemble_calls(page, permissions, identities, artifacts, budget)
}

fn assemble_calls(
    page: grapt_session::SessionToolCallPage,
    permissions: Vec<PermissionCall>,
    identities: Vec<ToolIdentity>,
    artifacts: &FilesystemArtifactStore,
    mut budget: Option<&mut usize>,
) -> Result<Vec<StoredCall>, recent::WindowError> {
    let identities_by_call: HashMap<&ToolCallId, &ToolIdentity> = identities
        .iter()
        .map(|identity| (&identity.call, identity))
        .collect();
    let permissions_by_call: HashMap<&ToolCallId, &PermissionCall> = permissions
        .iter()
        .map(|permission| (&permission.call, permission))
        .collect();
    let mut seen: HashSet<&ToolCallId> = HashSet::new();
    let mut calls: Vec<StoredCall> = Vec::with_capacity(page.rows().len());
    for row in page.rows() {
        if !seen.insert(row.call()) {
            return Err(format!(
                "tool {} has duplicate durable ToolCalled content",
                row.call()
            )
            .into());
        }
        let identity = identities_by_call.get(row.call()).copied();
        if identity
            .is_some_and(|held| held.message != *row.message() || held.seq >= row.committed().seq)
        {
            return Err("tool identity does not precede its own assistant call".into());
        }
        let permission = permissions_by_call.get(row.call()).copied();
        if identity
            .zip(permission)
            .is_some_and(|(identity, permission)| {
                !tool_matches_permission(&identity.tool, &permission.tool)
            })
        {
            return Err("tool identity conflicts with its permission action".into());
        }
        let tool = identity
            .map(|held| held.tool.clone())
            .or_else(|| permission.map(|held| held.tool.clone()));
        if let Some(budget) = budget.as_mut() {
            let charge = recent::TOOL_CALL_LABEL
                .len()
                .checked_add(tool.as_ref().map_or(0, |name| name.as_str().len()))
                .and_then(|value| value.checked_add(1))
                .and_then(|value| value.checked_add(row.payload().length() as usize))
                .and_then(|value| value.checked_add(1))
                .ok_or("recent tool calls overflow the request context bound")?;
            recent::charge_recent(
                budget,
                charge,
                "recent tool calls exceed the request context bound",
            )?;
        }
        let arguments = read_text(
            artifacts,
            row.payload(),
            row.disposition(),
            "tool arguments",
        )?;
        calls.push(StoredCall {
            seq: row.committed().seq,
            message: row.message().clone(),
            call: row.call().clone(),
            tool,
            permission_tool: permission.map(|held| held.tool.clone()),
            arguments,
            execution: identity.map_or(grapt_types::ToolExecution::Local, |identity| {
                identity.execution
            }),
            image_read: local_image_read(identity, permission),
        });
    }
    Ok(calls)
}

fn local_image_read(
    identity: Option<&ToolIdentity>,
    permission: Option<&PermissionCall>,
) -> Option<LocalImageRead> {
    let identity = identity?;
    if identity.execution != grapt_types::ToolExecution::Local
        || identity.tool.as_str() != crate::dogfood::tools::image::TOOL
    {
        return None;
    }
    match permission.map(|held| held.tool.as_str()) {
        Some("read") => Some(LocalImageRead::Permission),
        // Explicit local identities postdate builtin-first routing: a modern
        // local view_image cannot resolve to MCP/plugin code. Legacy v1 calls
        // lack that fact and still need the recorded read permission.
        None if identity.explicit_execution => Some(LocalImageRead::ExplicitLocalIdentity),
        _ => None,
    }
}

#[cfg(test)]
pub(super) fn stored_outputs(
    store: &EventStore,
    session: &SessionId,
    artifacts: &FilesystemArtifactStore,
    from: Option<grapt_types::EventSeq>,
    until: Option<grapt_types::EventSeq>,
) -> Result<Vec<StoredOutput>, String> {
    stored_outputs_for(
        store,
        session,
        artifacts,
        BoundedWindow {
            from,
            until,
            max_rows: MAX_CONTINUATION_ROWS,
        },
        None,
        None,
    )
    .map_err(String::from)
}

pub(super) fn stored_outputs_projected(
    store: &EventStore,
    session: &SessionId,
    artifacts: &FilesystemArtifactStore,
    from: Option<grapt_types::EventSeq>,
    until: Option<grapt_types::EventSeq>,
    calls: &[StoredCall],
) -> Result<Vec<StoredOutput>, String> {
    let rows = scoped_terminals(
        store,
        session,
        from,
        until,
        Scope::Model(None),
        MAX_CONTINUATION_ROWS,
    )?
    .into_iter()
    .map(|row| (row.seq, row.terminal))
    .collect();
    assemble_outputs(rows, artifacts, None, Some(calls), true).map_err(String::from)
}

pub(super) fn stored_outputs_for(
    store: &EventStore,
    session: &SessionId,
    artifacts: &FilesystemArtifactStore,
    window: BoundedWindow,
    target: Option<&ToolCallId>,
    budget: Option<(&mut usize, &[StoredCall])>,
) -> Result<Vec<StoredOutput>, recent::WindowError> {
    let rows = scoped_terminals(
        store,
        session,
        window.from,
        window.until,
        Scope::Model(target),
        window.max_rows,
    )?;
    let context_calls = if target.is_none() && budget.is_none() {
        Some(stored_calls_for(
            store, session, artifacts, window, None, None,
        )?)
    } else {
        None
    };
    let rows = rows
        .into_iter()
        .map(|row| (row.seq, row.terminal))
        .collect();
    assemble_outputs(rows, artifacts, budget, context_calls.as_deref(), false)
}

fn assemble_outputs(
    rows: Vec<(u64, StoredTerminal)>,
    artifacts: &FilesystemArtifactStore,
    budget: Option<(&mut usize, &[StoredCall])>,
    context_calls: Option<&[StoredCall]>,
    project_preview: bool,
) -> Result<Vec<StoredOutput>, recent::WindowError> {
    assemble_raw_outputs(rows, artifacts, budget, context_calls, project_preview)?
        .into_iter()
        .map(|raw| {
            Ok(StoredOutput {
                seq: raw.seq,
                call: raw.call,
                output: CodexToolText::try_new(raw.output).map_err(|error| error.to_string())?,
                error: raw.error,
            })
        })
        .collect()
}

struct OutputCallIndex<'a> {
    previewed_calls: HashMap<&'a ToolCallId, &'a str>,
    native_calls: HashMap<ToolCallId, (String, Option<String>)>,
    skill_calls: HashSet<ToolCallId>,
    image_calls: HashMap<ToolCallId, bool>,
    codemode_calls: HashSet<ToolCallId>,
}

impl<'a> OutputCallIndex<'a> {
    fn build(
        budget_calls: Option<&'a [StoredCall]>,
        context_calls: Option<&'a [StoredCall]>,
        project_preview: bool,
    ) -> Self {
        let previewed_calls = if project_preview {
            context_calls
                .into_iter()
                .flatten()
                .filter(|call| call.execution == grapt_types::ToolExecution::Local)
                .filter_map(|call| {
                    let tool = call.tool.as_ref()?.as_str();
                    shell_preview::is_previewed_tool(tool).then_some((&call.call, tool))
                })
                .collect()
        } else {
            HashMap::new()
        };
        let native_calls = budget_calls.map_or_else(HashMap::new, |calls| {
            calls
                .iter()
                .filter(|call| call.execution == grapt_types::ToolExecution::Local)
                .filter_map(|call| {
                    let name = call.tool.as_ref()?.as_str();
                    matches!(
                        name,
                        "shell" | "exec_command" | "request_sandbox_access" | "read"
                    )
                    .then(|| {
                        (
                            call.call.clone(),
                            (
                                name.to_owned(),
                                call.permission_tool
                                    .as_ref()
                                    .map(|tool| tool.as_str().to_owned()),
                            ),
                        )
                    })
                })
                .collect()
        });
        let skill_calls = budget_calls.map_or_else(HashSet::new, |calls| {
            calls
                .iter()
                .filter(|call| {
                    call.execution == grapt_types::ToolExecution::Local
                        && call
                            .tool
                            .as_ref()
                            .is_some_and(|tool| tool.as_str() == "skill")
                        && call
                            .permission_tool
                            .as_ref()
                            .is_none_or(|tool| tool.as_str() == "skill")
                })
                .map(|call| call.call.clone())
                .collect()
        });
        // Only native calls may interpret their output as our image receipt.
        // Explicit read provenance also makes missing/damaged markers corruption.
        let image_calls = budget_calls.map_or_else(HashMap::new, |calls| {
            calls
                .iter()
                .filter_map(|call| {
                    if call.execution != grapt_types::ToolExecution::Local
                        || call
                            .tool
                            .as_ref()
                            .is_none_or(|tool| tool.as_str() != crate::dogfood::tools::image::TOOL)
                    {
                        return None;
                    }
                    match call.permission_tool.as_ref().map(|tool| tool.as_str()) {
                        Some("read") => Some((call.call.clone(), true)),
                        None => Some((call.call.clone(), call.image_read.is_some())),
                        Some(_) => None,
                    }
                })
                .collect()
        });
        let codemode_calls = context_calls
            .or(budget_calls)
            .map_or_else(HashSet::new, |calls| {
                calls
                    .iter()
                    .filter(|call| {
                        crate::dogfood::tools::codemode_failure::owns(
                            call.tool.as_ref().map_or("", |tool| tool.as_str()),
                            call.execution,
                            call.permission_tool.as_ref().map(|tool| tool.as_str()),
                        )
                    })
                    .map(|call| call.call.clone())
                    .collect()
            });
        Self {
            previewed_calls,
            native_calls,
            skill_calls,
            image_calls,
            codemode_calls,
        }
    }
}

fn codemode_public_cause(
    artifacts: &FilesystemArtifactStore,
    artifact: Option<&grapt_types::ToolTerminalArtifact>,
    failure_reason: Option<grapt_types::ToolFailureReason>,
) -> Result<Option<String>, String> {
    match (artifact, failure_reason) {
        (Some(artifact), Some(reason)) => {
            // Artifact reads verify a whole chunk before exposing a prefix.
            let bound = ReadBudget::new(65_536).ok_or("CodeMode cause read bound invalid")?;
            // An unreadable cause loses its detail, never the turn.
            let prefix = artifacts.fetch(artifact.reference(), 0, bound);
            let prefix = prefix.as_ref().map_or(&[][..], |page| page.bytes());
            Ok(crate::dogfood::tools::codemode_failure::public(
                reason,
                &String::from_utf8_lossy(&prefix[..prefix.len().min(2048)]),
            ))
        }
        (None, Some(reason)) => Ok(crate::dogfood::tools::codemode_failure::public(reason, "")),
        _ => Ok(None),
    }
}

fn resolve_budgeted_error(
    reads: &OutputReads<'_>,
    index: &OutputCallIndex<'_>,
    fact: &session_tool_terminal::ToolTerminalFact,
    public_cause: Option<String>,
    error: &str,
    budget: &mut usize,
) -> Result<String, recent::WindowError> {
    let call = fact.call();
    let (artifact, failure_reason) = match fact.outcome() {
        ToolTerminalOutcome::Failed { reason, detail } => (detail.as_ref(), Some(*reason)),
        _ => (None, None),
    };
    // Only a complete small diagnostic from the owned skill action is eligible;
    // arbitrary artifact text stays private.
    let detail = if let Some(cause) = public_cause {
        Some(cause)
    } else if let Some(artifact) = artifact {
        if failure_reason == Some(grapt_types::ToolFailureReason::EffectFailed)
            && index.skill_calls.contains(call)
            && artifact.reference().length() <= 1024
        {
            let text = reads.text(
                artifact.reference(),
                artifact.disposition(),
                "skill failure",
            )?;
            crate::dogfood::tools::public_missing_skill_detail(text.as_str()).map(str::to_owned)
        } else if let Some((tool, action)) = index.native_calls.get(call).filter(|_| {
            matches!(
                failure_reason,
                Some(
                    grapt_types::ToolFailureReason::EffectFailed
                        | grapt_types::ToolFailureReason::EffectUnknown
                        | grapt_types::ToolFailureReason::AuthorityUnavailable
                )
            ) && artifact.reference().length()
                <= crate::dogfood::tools::native_failure::MAX_BYTES as u64
        }) {
            let text = reads.text(
                artifact.reference(),
                artifact.disposition(),
                "native failure",
            )?;
            crate::dogfood::tools::native_failure::public_detail(
                tool,
                action.as_deref(),
                true,
                text.as_str(),
            )
            .map(str::to_owned)
        } else {
            None
        }
    } else {
        None
    };
    let detail = detail
        .filter(|text| !text.is_empty())
        .map(|text| crate::dogfood::path_display::failure_text(&text).into_owned());
    let charge = recent::TOOL_ERROR_LABEL
        .len()
        .checked_add(error.len())
        .and_then(|value| value.checked_add(detail.as_ref().map_or(0, |text| text.len() + 1)))
        .and_then(|value| value.checked_add(1))
        .ok_or("recent tool outputs overflow the request context bound")?;
    recent::charge_recent(
        budget,
        charge,
        "recent tool outputs exceed the request context bound",
    )?;
    Ok(detail.unwrap_or_default())
}

fn resolve_budgeted_success(
    reads: &OutputReads<'_>,
    image_calls: &HashMap<ToolCallId, bool>,
    call: &ToolCallId,
    artifact: &grapt_types::ToolTerminalArtifact,
    budget: &mut usize,
) -> Result<String, recent::WindowError> {
    let reference = artifact.reference();
    let charged_units = if reference.length() <= recent::TOOL_OUTPUT_MAX_UNITS as u64 {
        reference.length() as usize
    } else {
        recent::MAX_TRUNCATED_RESULT_BYTES
    };
    let charge = recent::TOOL_RESULT_LABEL
        .len()
        .checked_add(charged_units)
        .and_then(|value| value.checked_add(1))
        .ok_or("recent tool outputs overflow the request context bound")?;
    recent::charge_recent(
        budget,
        charge,
        "recent tool outputs exceed the request context bound",
    )?;
    let bytes = read_first_chunk(reads, reference, artifact.disposition(), "tool output")?;
    let text =
        String::from_utf8(bytes).map_err(|_| "tool output artifact is not UTF-8".to_owned())?;
    // A notice standing in for an unreadable output is no receipt.
    if !reads.unreadable()
        && image_calls
            .get(call)
            .is_some_and(|proven| *proven || crate::dogfood::tools::image::has_receipt(&text))
    {
        let full = reads.text(reference, artifact.disposition(), "image output")?;
        // A later chunk can fail after the first verified.
        if reads.unreadable() {
            Ok(full.as_str().to_owned())
        } else {
            match crate::dogfood::tools::image::replay(full.as_str()) {
                Ok((description, _)) => Ok(description.as_str().to_owned()),
                // A receipt that does not parse is a lost output, as one failing its digest is.
                Err(error) => {
                    crate::diagnostics::report(format!(
                        "native image receipt of {call} is corrupt: {error}"
                    ));
                    Ok(reads.notice("image output"))
                }
            }
        }
    } else {
        Ok(text)
    }
}

fn assemble_raw_outputs(
    rows: Vec<(u64, StoredTerminal)>,
    artifacts: &FilesystemArtifactStore,
    mut budget: Option<(&mut usize, &[StoredCall])>,
    context_calls: Option<&[StoredCall]>,
    project_preview: bool,
) -> Result<Vec<RawStoredOutput>, recent::WindowError> {
    let index = OutputCallIndex::build(
        budget.as_ref().map(|(_, calls)| *calls),
        context_calls,
        project_preview,
    );
    let mut outputs = Vec::new();
    for (seq, data) in rows {
        let StoredTerminal::Readable(fact) = data else {
            continue;
        };
        let reads = OutputReads::new(artifacts);
        let (artifact, error, failure_reason) = match fact.outcome() {
            ToolTerminalOutcome::Completed { output } => (output.as_ref(), None, None),
            ToolTerminalOutcome::Failed { reason, detail } => {
                (detail.as_ref(), Some(failure_line(*reason)), Some(*reason))
            }
            ToolTerminalOutcome::Cancelled { by } => {
                (None, Some(format!("Tool cancelled: {by:?}")), None)
            }
            ToolTerminalOutcome::ResultMissing => {
                (None, Some("The tool result is missing".to_owned()), None)
            }
        };
        let public_cause = if index.codemode_calls.contains(fact.call()) {
            codemode_public_cause(artifacts, artifact, failure_reason)?
        } else {
            None
        };
        let text = match budget.as_mut() {
            Some((budget, _)) => match (artifact, &error) {
                (_, Some(error)) => {
                    resolve_budgeted_error(&reads, &index, &fact, public_cause, error, budget)?
                }
                (Some(artifact), None) => resolve_budgeted_success(
                    &reads,
                    &index.image_calls,
                    fact.call(),
                    artifact,
                    budget,
                )?,
                (None, None) => String::new(),
            },
            None if public_cause.is_some() => public_cause.unwrap_or_default(),
            None => match artifact {
                Some(artifact) => {
                    reads.string(artifact.reference(), artifact.disposition(), "tool output")?
                }
                None => String::new(),
            },
        };
        let error = error.or_else(|| reads.unreadable().then(|| UNREADABLE.to_owned()));
        let text = match &error {
            Some(error) if text.is_empty() => error.clone(),
            Some(error) => format!("{error}\n{text}"),
            None => text,
        };
        let text = if error.is_some() {
            crate::dogfood::path_display::failure_text(&text).into_owned()
        } else {
            text
        };
        let output_text = if let Some(tool) = index.previewed_calls.get(fact.call()) {
            shell_preview::project_str(tool, fact.call(), &text)?
                .as_str()
                .to_owned()
        } else {
            text
        };
        outputs.push(RawStoredOutput {
            capture_failed: matches!(
                fact.outcome(),
                ToolTerminalOutcome::Failed {
                    reason: grapt_types::ToolFailureReason::CaptureFailed,
                    ..
                }
            ),
            seq,
            call: fact.call().clone(),
            output: output_text,
            error,
            failure_reason,
        });
    }
    Ok(outputs)
}

/// The failure line of an output whose recorded artifact cannot be read.
const UNREADABLE: &str = "The recorded tool output is unavailable or corrupt";

fn read_first_chunk(
    reads: &OutputReads<'_>,
    reference: &grapt_types::ArtifactRef,
    disposition: CaptureDisposition,
    label: &str,
) -> Result<Vec<u8>, String> {
    if !disposition.is_complete() {
        return Err(format!("{label} artifact is truncated"));
    }
    let max_bytes = tool_artifact_read_bound();
    if reference.length() > max_bytes as u64 {
        return Err(format!("{label} artifact exceeds the continuation bound"));
    }
    const FIRST_CHUNK_BYTES: u64 = 65_536;
    let (artifacts, unreadable) = (reads.artifacts, || reads.notice(label).into_bytes());
    if reference.length() <= FIRST_CHUNK_BYTES {
        return Ok(
            fetch_bytes_bounded(artifacts, reference, disposition, label, max_bytes)?
                .unwrap_or_else(|_| unreadable()),
        );
    }
    let budget = ReadBudget::new(FIRST_CHUNK_BYTES)
        .ok_or_else(|| format!("{label} artifact read bound is zero"))?;
    let page = match artifacts.fetch(reference, 0, budget) {
        Ok(page) => page,
        Err(grapt_artifact::FetchError::BudgetTooSmall { minimum }) => {
            if minimum > max_bytes as u64 {
                return Err(format!("{label} artifact exceeds the continuation bound"));
            }
            let retry = ReadBudget::new(minimum)
                .ok_or_else(|| format!("{label} artifact read bound is zero"))?;
            match artifacts.fetch(reference, 0, retry) {
                Ok(page) => page,
                Err(_) => return Ok(unreadable()),
            }
        }
        Err(_) => return Ok(unreadable()),
    };
    let bytes = page.bytes().to_vec();
    match std::str::from_utf8(&bytes) {
        Ok(_) => Ok(bytes),
        Err(error) => Ok(bytes[..error.valid_up_to()].to_vec()),
    }
}

#[cfg(test)]
mod apply_patch_alias_tests {
    use super::*;

    #[test]
    fn native_patch_action_replays_without_accepting_foreign_pairs() -> Result<(), String> {
        let tool = ToolName::new("apply_patch").ok_or("patch tool")?;
        let action = ToolName::new("patch").ok_or("patch action")?;
        assert!(tool_matches_permission(&tool, &action));
        for name in ["mcp.apply_patch", "plugin.apply_patch", "read"] {
            let foreign = ToolName::new(name).ok_or("foreign tool")?;
            assert!(!tool_matches_permission(&foreign, &action));
        }
        let wrong = ToolName::new("read").ok_or("wrong action")?;
        assert!(!tool_matches_permission(&tool, &wrong));
        Ok(())
    }
}

#[cfg(test)]
mod terminal_alias_tests {
    use super::*;
    #[test]
    fn owned_process_and_poll_actions_replay_without_admitting_foreign_aliases(
    ) -> Result<(), String> {
        for (name, action, expected) in [
            ("exec_command", "shell", true),
            ("write_stdin", "shell", true),
            ("write_stdin", "shell.poll", true),
            ("exec_command", "shell.poll", false),
            ("write_stdin", "read", false),
            ("mcp_stdio", "shell.poll", false),
            ("read_history", "history", true),
            ("read_history", "read", false),
            ("mcp.read_history", "history", false),
        ] {
            let name = ToolName::new(name).ok_or("tool")?;
            let action = ToolName::new(action).ok_or("action")?;
            assert_eq!(tool_matches_permission(&name, &action), expected);
        }
        Ok(())
    }
}
