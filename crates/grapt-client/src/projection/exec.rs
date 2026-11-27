//! Public terminal reports become literal output only in client presentation.
use serde_json::Value;

pub(super) fn present(name: &str, raw: &str) -> Option<String> {
    if !matches!(
        name,
        "exec_command" | "write_stdin" | "request_sandbox_access"
    ) {
        return None;
    }
    let value: Value = serde_json::from_str(raw).ok()?;
    let output = super::tool_summary::output_text(&value)?;
    let done = value.get("done")?.as_bool()?;
    let more = value.get("has_more_output")?.as_bool()?;
    let omitted = value.get("omitted_bytes")?.as_u64()?;
    let status = value.get("status")?.as_str()?;
    let mut summary = match status {
        "exited" => format!("exit {}", value.get("exit_code")?.as_i64()?),
        "running" if !done => "running".into(),
        "timed_out" => "timed out".into(),
        "cancelled" => "cancelled".into(),
        "failed" => "failed".into(),
        _ => return None,
    };
    if !done && status != "running" {
        summary.push_str(" · completion pending");
    }
    if omitted > 0 {
        summary.push_str(&format!(" · {omitted} output bytes omitted"));
    }
    if more {
        summary.push_str(" · more output available");
    }
    let mut shown = format!("\n  {summary}");
    for key in ["error", "cleanup_error", "interrupt", "retry_guidance"] {
        if let Some(reason) = value.get(key) {
            shown.push_str(&format!(
                "\n  {key}: {}",
                super::read::display_line(reason.as_str()?)
            ));
        }
    }
    for line in output.lines() {
        shown.push_str("\n  ");
        shown.push_str(&super::read::display_line(line));
    }
    Some(shown)
}

#[cfg(test)]
mod tests;
