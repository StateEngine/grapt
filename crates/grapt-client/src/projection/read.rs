//! Read-result presentation belongs to the client; wire/model output stays raw.
use crate::wire::{DurableTool, DurableToolState};

const PREVIEW_LINES: usize = 12;

pub(super) fn present(tool: &DurableTool) -> Option<(String, usize)> {
    if !matches!(tool.name.as_str(), "read" | "read_lines") {
        return None;
    }
    let DurableToolState::Completed { input, output } = &tool.state else {
        return None;
    };
    let input: serde_json::Value = serde_json::from_str(input).ok()?;
    let path = display_line(input.get("path")?.as_str()?);
    let lines = if tool.name == "read_lines" {
        read_lines(output)
    } else {
        numbered_lines(output)
    };
    let Some((rows, next)) = lines else {
        let summary = format!("Read {path} · output format unavailable");
        let start = summary.len();
        let mut lines = output.lines();
        let raw = lines
            .by_ref()
            .take(PREVIEW_LINES)
            .map(display_line)
            .collect::<Vec<_>>()
            .join("\n");
        let omitted = if lines.next().is_some() {
            "\n… remaining output omitted"
        } else {
            ""
        };
        return Some((
            format!("{summary}\nRaw output preview:\n{raw}{omitted}"),
            start,
        ));
    };
    let summary = match (rows.first(), rows.last()) {
        (Some(first), Some(last)) => format!(
            "Read {path} · lines {}–{} ({} lines)",
            first.0,
            last.0,
            rows.len()
        ),
        _ => format!("Read {path} · 0 lines"),
    };
    let start = summary.len();
    let mut shown = summary;
    for (number, text) in rows.iter().take(PREVIEW_LINES) {
        shown.push_str(&format!("\n{number}: {}", display_line(text)));
    }
    if rows.len() > PREVIEW_LINES {
        shown.push_str(&format!(
            "\n… {} more lines omitted",
            rows.len() - PREVIEW_LINES
        ));
    }
    if let Some((next, column)) = next {
        shown.push_str(&format!("\nContinue reading at line {next}"));
        if column > 1 {
            shown.push_str(&format!(", column {column}"));
        }
    }
    Some((shown, start))
}

// Decoded JSON can contain terminal control bytes that were escaped on wire.
// Make them visible without changing the raw tool output or logical row count.
pub(super) fn display_line(text: &str) -> String {
    let mut shown = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_control() {
            shown.extend(ch.escape_debug());
        } else {
            shown.push(ch);
        }
    }
    shown
}

type Lines = (Vec<(u64, String)>, Option<(u64, u64)>);

fn read_lines(output: &str) -> Option<Lines> {
    json_lines(output).or_else(|| compact_lines(output))
}

/// Decode the product's compact text content, not an extra JSON string wrapper.
/// Header/hash and scoped-instruction metadata remain in the raw wire output.
fn compact_lines(output: &str) -> Option<Lines> {
    let mut lines = output.lines();
    let header = lines.next()?.strip_prefix("File: ")?;
    let (identity, origin) = header.rsplit_once(" | start_line: ")?;
    let (line, _) = read_cursor(origin, " start_column: ")?;
    let (identity, total) = identity.rsplit_once(" | total_lines: ")?;
    let total = total.parse::<u64>().ok()?;
    let (path, hash) = identity.rsplit_once(" | sha256: ")?;
    serde_json::from_str::<String>(path).ok()?;
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut rows = Vec::new();
    let mut expected = line;
    for line in lines {
        if line == "[end of file]" {
            return (expected > total).then_some((rows, None));
        }
        if let Some(cursor) = line
            .strip_prefix("[continue: start_line=")
            .and_then(|text| text.strip_suffix(']'))
        {
            let (next, column) = read_cursor(cursor, " start_column=")?;
            let valid = if column == 1 {
                next == expected
            } else {
                rows.last().is_some_and(|(last, _)| *last == next)
            };
            return (valid && next <= total).then_some((rows, Some((next, column))));
        }
        let (number, text) = line.split_once('→')?;
        let number = number.parse::<u64>().ok()?;
        if number != expected || number > total || rows.len() >= 2000 {
            return None;
        }
        expected = number.checked_add(1)?;
        rows.push((number, text.to_owned()));
    }
    // A missing terminal marker is incomplete output, not an empty/completed read.
    None
}

fn read_cursor(text: &str, separator: &str) -> Option<(u64, u64)> {
    let (line, column) = text.split_once(separator)?;
    let line = line.parse::<u64>().ok()?;
    let column = column.parse::<u64>().ok()?;
    (line > 0 && column > 0).then_some((line, column))
}

fn json_lines(output: &str) -> Option<Lines> {
    let value: serde_json::Value = serde_json::from_str(output).ok()?;
    let total = value.get("totalLines")?.as_u64()?;
    let complete = value.get("complete")?.as_bool()?;
    let next = match value.get("nextLine")? {
        serde_json::Value::Null => None,
        value => Some(value.as_u64()?),
    };
    if complete != next.is_none() {
        return None;
    }
    let values = value.get("lines")?.as_array()?;
    if values.len() > 2000 {
        return None;
    }
    let mut rows = Vec::with_capacity(values.len());
    let mut previous = None::<u64>;
    for row in values {
        let number = row.get("line")?.as_u64()?;
        let text = row.get("text")?.as_str()?;
        if number == 0
            || number > total
            || text.contains(['\n', '\r'])
            || previous.is_some_and(|before| before.checked_add(1) != Some(number))
        {
            return None;
        }
        previous = Some(number);
        rows.push((number, text.to_owned()));
    }
    if next.is_some_and(|next| {
        next == 0 || next > total || previous.and_then(|n| n.checked_add(1)) != Some(next)
    }) {
        return None;
    }
    Some((rows, next.map(|line| (line, 1))))
}

fn numbered_lines(output: &str) -> Option<Lines> {
    let mut rows = Vec::new();
    let mut next = None;
    for line in output.lines() {
        if let Some(value) = line.strip_prefix("[Output truncated. Continue reading with offset: ")
        {
            let following = value.strip_suffix(']')?.parse::<u64>().ok()?;
            if following == 0
                || rows
                    .last()
                    .and_then(|(number, _): &(u64, String)| number.checked_add(1))
                    != Some(following)
            {
                return None;
            }
            next = Some(following);
            break;
        }
        if line.starts_with("[sha256 for patch:") {
            break;
        }
        let (number, text) = line.split_once(": ")?;
        let number = number.parse::<u64>().ok()?;
        if number == 0
            || rows.len() >= 2000
            || rows
                .last()
                .is_some_and(|(before, _): &(u64, String)| before.checked_add(1) != Some(number))
        {
            return None;
        }
        rows.push((number, text.to_owned()));
    }
    Some((rows, next.map(|line| (line, 1))))
}

#[cfg(test)]
mod tests;

/// Use the same numbered-read decoder for the compact tool output preview.
pub(super) fn tool_preview(tool: &DurableTool) -> Option<(Vec<String>, usize)> {
    let DurableToolState::Completed { output, .. } = &tool.state else {
        return None;
    };
    let (rows, _) = match tool.name.as_str() {
        "read" => numbered_lines(output)?,
        "read_lines" => read_lines(output)?,
        _ => return None,
    };
    let shown = rows
        .iter()
        .take(3)
        .map(|(number, text)| format!("{number}: {}", super::tool_summary::compact(text, 500)))
        .collect();
    Some((shown, rows.len().saturating_sub(3)))
}
