//! A bounded preview of output; expansion continues to use retained tool detail.
use super::*;
const ROWS: usize = 3;
pub(super) fn build(tool: &DurableTool, output: &str) -> (Vec<String>, usize) {
    if let Some(changes) = changes(tool) {
        return changes;
    }
    if let Some(read) = super::super::read::tool_preview(tool) {
        return read;
    }
    let value: Option<Value> = serde_json::from_str(output).ok();
    let text = value.as_ref().and_then(output_text);
    let combined = value
        .as_ref()
        .filter(|value| value.get("stdout").is_some() || value.get("stderr").is_some())
        .map(|value| {
            let stdout = value
                .get("stdout")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let stderr = value
                .get("stderr")
                .and_then(Value::as_str)
                .unwrap_or_default();
            format!(
                "{stdout}{}{stderr}",
                if !stdout.is_empty() && !stdout.ends_with('\n') && !stderr.is_empty() {
                    "\n"
                } else {
                    ""
                }
            )
        });
    if let Some(matches) = value
        .as_ref()
        .and_then(|value| value.get("matches"))
        .and_then(Value::as_array)
    {
        let shown = matches
            .iter()
            .take(ROWS)
            .map(|row| {
                let path = row
                    .get("path")
                    .or_else(|| row.get("file"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let line = row.get("line").and_then(Value::as_u64);
                let text = row.get("text").and_then(Value::as_str).unwrap_or_default();
                clean(&format!(
                    "{path}{}: {text}",
                    line.map(|number| format!(":{number}")).unwrap_or_default()
                ))
            })
            .collect();
        return (shown, matches.len().saturating_sub(ROWS));
    }
    let text = text.or(combined.as_deref()).unwrap_or(output).trim();
    let count = text.lines().count();
    (
        text.lines().take(ROWS).map(clean).collect(),
        count.saturating_sub(ROWS),
    )
}

fn clean(text: &str) -> String {
    let mut shown = String::new();
    let mut escape = 0;
    for ch in text.chars() {
        match escape {
            0 if ch == '\x1b' => escape = 1,
            0 => shown.push(ch),
            1 if ch == '[' => escape = 2,
            1 if ch == ']' => escape = 3,
            1 => escape = 0,
            2 if ('@'..='~').contains(&ch) => escape = 0,
            3 if ch == '\x07' => escape = 0,
            3 if ch == '\x1b' => escape = 4,
            4 if ch == '\\' => escape = 0,
            4 => escape = 3,
            _ => {}
        }
    }
    compact(&shown, 512)
}

fn changes(tool: &DurableTool) -> Option<(Vec<String>, usize)> {
    if !matches!(
        tool.name.as_str(),
        "edit" | "write" | "patch" | "apply_patch"
    ) {
        return None;
    }
    let (text, marks) = super::super::diff::present(tool, "")?;
    let lines: Vec<_> = text
        .lines()
        .zip(marks)
        .filter(|(_, mark)| {
            matches!(
                mark,
                super::super::Mark::Insert
                    | super::super::Mark::Delete
                    | super::super::Mark::Context
            )
        })
        .map(|(line, _)| clean(line))
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some((
        lines.iter().take(ROWS).cloned().collect(),
        lines.len().saturating_sub(ROWS),
    ))
}
