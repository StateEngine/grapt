// Copyright 2025 OpenAI. Apache-2.0. Modified by Grapt.
//! Empty stdin is a wait. Reuse retained command titles.
use super::*;

pub(super) fn action(tool: &DurableTool, input: Option<&Value>) -> Option<StdinAction> {
    if tool.name != "write_stdin" {
        return None;
    }
    let input = input?.as_object()?;
    let chars = match input.get("chars") {
        None => "",
        Some(value) => value.as_str()?,
    };
    Some(if chars.is_empty() {
        StdinAction::Wait
    } else {
        StdinAction::Write
    })
}

pub(super) fn session(
    tool: &DurableTool,
    input: Option<&Value>,
    output: Option<&str>,
) -> Option<grapt_types::OperationId> {
    let output_value;
    let source = match tool.name.as_str() {
        "write_stdin" => input?,
        "exec_command" | "request_sandbox_access" => {
            output_value = serde_json::from_str::<Value>(output?).ok()?;
            &output_value
        }
        _ => return None,
    };
    let raw = source.get("session_id")?.as_str()?;
    if raw.len() > 256 || raw.chars().any(char::is_control) {
        return None;
    }
    grapt_types::OperationId::new(raw)
}

pub(super) fn title(action: StdinAction, command: Option<&str>) -> String {
    let command = command
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("the running command");
    let command = compact(command, 100);
    match action {
        StdinAction::Wait => format!("Waited for {command} output"),
        StdinAction::Write => format!("Wrote to {command}"),
    }
}

pub(super) fn resolve_title(summary: &mut ToolSummary, slices: &[super::super::Slice]) {
    let Some(action) = summary.stdin else {
        return;
    };
    let command = summary.terminal_session.as_ref().and_then(|session| {
        slices
            .iter()
            .rev()
            .filter_map(|slice| slice.tool_summary.as_ref())
            .find(|tool| {
                matches!(
                    tool.name.as_str(),
                    "exec_command" | "request_sandbox_access"
                ) && tool.terminal_session.as_ref() == Some(session)
            })
            .map(|tool| {
                tool.title
                    .strip_prefix("Ran ")
                    .or_else(|| tool.title.strip_prefix("Running "))
                    .unwrap_or(&tool.title)
            })
    });
    summary.title = title(action, command);
}
