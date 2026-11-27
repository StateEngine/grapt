//! Derive a short human action when the model did not supply a description.
use super::*;
pub(super) fn title(tool: &DurableTool, input: Option<&Value>, argument: &str) -> String {
    if let Some(action) = super::terminal::action(tool, input) {
        return super::terminal::title(action, None);
    }
    let get = |key| {
        input
            .and_then(|value| value.get(key))
            .and_then(Value::as_str)
            .unwrap_or_default()
    };
    let description = get("description").trim();
    if !description.is_empty() {
        return compact(description, 160);
    }
    let running = matches!(
        tool.state,
        DurableToolState::Streaming { .. } | DurableToolState::Running { .. }
    );
    let verb = |active, done| if running { active } else { done };
    let target = compact(argument.lines().next().unwrap_or_default(), 100);
    let action = match tool.name.as_str() {
        "request_sandbox_access" if !get("host").is_empty() => format!(
            "{} host access: {}",
            verb("Requesting", "Requested"),
            compact(get("host"), 100)
        ),
        "request_sandbox_access" => format!(
            "{} with write access: {}",
            verb("Retrying", "Retried"),
            compact(get("path"), 100)
        ),
        "shell" | "exec_command" => format!(
            "{} {}",
            verb("Running", "Ran"),
            target.split_whitespace().next().unwrap_or("command")
        ),
        "execute" => format!("{} code", verb("Running", "Ran")),
        "read" | "read_lines" => format!("{} {target}", verb("Reading", "Read")),
        "grep" => {
            let pattern = compact(get("pattern"), 80);
            let path = compact(get("path"), 80);
            format!(
                "{} {pattern}{}",
                verb("Searching", "Searched"),
                if path.is_empty() {
                    String::new()
                } else {
                    format!(" in {path}")
                }
            )
        }
        "glob" | "list" => format!("{} {target}", verb("Listing", "Listed")),
        "edit" => format!("{} {target}", verb("Editing", "Edited")),
        "write" => format!("{} {target}", verb("Writing", "Wrote")),
        "patch" | "apply_patch" => {
            let patch_path = get("patchText")
                .lines()
                .find_map(|line| {
                    ["*** Update File: ", "*** Add File: ", "*** Delete File: "]
                        .iter()
                        .find_map(|prefix| line.strip_prefix(*prefix))
                })
                .unwrap_or_default();
            let operation_path = input
                .and_then(|value| value.get("operations"))
                .and_then(Value::as_array)
                .and_then(|operations| operations.first())
                .and_then(|operation| operation.get("path"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let path = if !patch_path.is_empty() {
                patch_path
            } else if !operation_path.is_empty() {
                operation_path
            } else {
                target.as_str()
            };
            format!(
                "{} {}",
                verb("Patching", "Patched"),
                if path.is_empty() { "files" } else { path }
            )
        }
        "write_stdin" => "Interacted with the running command".into(),
        "update_plan" => "Updated plan".into(),
        name if name.starts_with("stateengine_") => {
            format!("{} stateengine", verb("Calling", "Called"))
        }
        name => format!(
            "{} {} {target}",
            verb("Calling", "Called"),
            compact(name, 80)
        ),
    };
    compact(action.trim(), 160)
}
