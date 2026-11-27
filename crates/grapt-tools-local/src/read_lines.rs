//! Lossless, bounded UTF-8 source pages over application-authorized file paths.

use std::path::Path;

use crate::filesystem::{is_link, read_bounded, reject_links};
use crate::search::OUTPUT_BYTES;
use crate::FILE_EFFECT_MAX_BYTES;

mod render;
pub use render::encode_page;

#[cfg(test)]
mod tests;

/// Row budget per page; the rest carries metadata and scoped instructions.
const PAGE_BYTES: usize = OUTPUT_BYTES * 3 / 4;

/// Build one page of an application-authorized file.
///
/// Coordinates are 1-based. This reader refuses zero itself rather than
/// leaving the bound to the caller: `start_line` and `start_column` are
/// decremented below, which an unchecked zero would underflow.
pub fn page(
    root: &Path,
    path: &Path,
    start_line: u64,
    start_column: u64,
    line_count: u64,
) -> Result<serde_json::Value, String> {
    if start_line == 0 || start_column == 0 {
        return Err("read_lines requires 1-based start coordinates".into());
    }
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "read_lines path escaped workspace".to_owned())?;
    reject_links(root, relative)?;
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("read_lines metadata unavailable: {error}"))?;
    if !metadata.is_file() || is_link(&metadata) {
        return Err("read_lines requires a regular file without linked components".into());
    }
    if metadata.len() > FILE_EFFECT_MAX_BYTES {
        return Err("read_lines file byte bound exceeded".into());
    }
    let bytes = read_bounded(path, FILE_EFFECT_MAX_BYTES)
        .map_err(|error| format!("read_lines unavailable: {error}"))?;
    if bytes.len() as u64 > FILE_EFFECT_MAX_BYTES {
        return Err("read_lines file byte bound exceeded".into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        format!(
            "read_lines content is not UTF-8 at byte {}",
            error.valid_up_to()
        )
    })?;
    if text.contains('\0') {
        return Err("read_lines refuses binary NUL content".into());
    }
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let total_lines = u64::try_from(lines.len()).map_err(|_| "read_lines count exceeded u64")?;
    let start_index =
        usize::try_from(start_line - 1).map_err(|_| "read_lines start exceeds usize")?;
    let count = usize::try_from(line_count).map_err(|_| "read_lines count exceeds usize")?;
    let mut end_index = start_index.saturating_add(count).min(lines.len());
    let mut rows = Vec::new();
    let mut offset = 0_usize;
    let mut rendered = 0_usize;
    let mut next_column = None;
    for (index, line) in lines.iter().enumerate() {
        if index >= end_index {
            break;
        }
        let end = offset + line.len();
        if index >= start_index {
            let text = line.trim_end_matches(['\n', '\r']);
            let column = if index == start_index {
                start_column
            } else {
                1
            };
            let start = column_byte(text, column)?;
            let text = &text[start..];
            let row = serde_json::json!({
                "line":index + 1,
                "byteStart":offset + start,
                "byteEnd":end,
                "text":text,
            });
            let size = row.to_string().len() + 1;
            if rendered + size > PAGE_BYTES {
                // Keep ordinary rows whole. A first row that cannot fit is
                // split at a Unicode boundary, with a cursor in the same line.
                if rows.is_empty() {
                    let (prefix, characters) = line_prefix(index + 1, offset + start, text)?;
                    rows.push(prefix);
                    next_column = Some(column + characters);
                }
                end_index = index;
                break;
            }
            rendered += size;
            rows.push(row);
        }
        offset = end;
    }
    let next_line = if end_index < lines.len() {
        Some(end_index + 1)
    } else {
        None
    };
    let output = serde_json::json!({
        "path":relative.to_str().ok_or("read_lines path is not UTF-8")?.replace('\\', "/"),
        "lines":rows,
        "sha256":ring::digest::digest(&ring::digest::SHA256, &bytes)
            .as_ref().iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
        "totalLines":total_lines,
        "nextLine":next_line,
        "nextColumn":next_line.map(|_| next_column.unwrap_or(1)),
        "complete":next_line.is_none(),
    });
    Ok(output)
}

/// Convert a character column to a byte boundary without interpreting a UTF-8
/// continuation byte as a column. One-past-end is a valid empty suffix.
fn column_byte(text: &str, column: u64) -> Result<usize, String> {
    let skip = usize::try_from(column - 1).map_err(|_| "read_lines column exceeds usize")?;
    let mut characters = text.char_indices();
    for _ in 0..skip {
        if characters.next().is_none() {
            return Err("read_lines start_column is past the end of the selected line".into());
        }
    }
    Ok(characters.next().map_or(text.len(), |(byte, _)| byte))
}

/// Find the largest first-line prefix whose serialized row fits. Escaped
/// quotes/control characters count toward the bound, not just source bytes.
fn line_prefix(
    number: usize,
    offset: usize,
    text: &str,
) -> Result<(serde_json::Value, u64), String> {
    let row = |end| {
        serde_json::json!({
            "line":number, "byteStart":offset, "byteEnd":offset + end, "text":&text[..end],
        })
    };
    let mut low = 0;
    // A partial row must leave source characters for its continuation, even
    // when excluding the newline shortens the serialized byteEnd by a digit.
    let mut high = text.char_indices().next_back().map_or(0, |(byte, _)| byte);
    while low < high {
        let mut end = low + (high - low).div_ceil(2);
        while !text.is_char_boundary(end) {
            end += 1;
        }
        if row(end).to_string().len() < PAGE_BYTES {
            low = end;
        } else {
            high = end - 1;
            while !text.is_char_boundary(high) {
                high -= 1;
            }
        }
    }
    if low == 0 {
        return Err("read_lines page cannot hold one source character".into());
    }
    let characters = u64::try_from(text[..low].chars().count())
        .map_err(|_| "read_lines column count exceeds u64")?;
    Ok((row(low), characters))
}
