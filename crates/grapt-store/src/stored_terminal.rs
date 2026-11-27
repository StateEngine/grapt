//! Reads stored tool terminals without trusting their size or encoding.
//!
//! Every Session reader selects a terminal's data through [`bounded_column`]
//! and classifies it with [`classify`], so an oversized row is never fetched
//! and text that is not UTF-8 is unreadable evidence, never a failed read.
//! An oversized row keeps only its call identity, as a hint for warnings.

use grapt_event::session_tool_terminal::{StoredTerminal, MAX_STORED_TERMINAL_BYTES};
use rusqlite::types::ValueRef;

/// SQL selecting terminal `column` as bytes. Past the bound it yields only
/// `{"call": ...}`, the row's text call identity, or the empty blob when even
/// that is past the bound: no decoder accepts either, but the call stays
/// named. Readers shorten an identity only where they display it.
pub fn bounded_column(column: &str) -> String {
    let hint = format!("json_object('call', json_extract({column}, '$.call'))");
    format!(
        "CASE WHEN length(CAST({column} AS BLOB)) <= {MAX_STORED_TERMINAL_BYTES}
              THEN CAST({column} AS BLOB)
              WHEN json_valid({column}) AND json_type({column}, '$.call') = 'text'
               AND length(CAST({hint} AS BLOB)) <= {MAX_STORED_TERMINAL_BYTES}
              THEN CAST({hint} AS BLOB)
              ELSE X'' END"
    )
}

/// Classifies the column at `index`, borrowing the bytes SQLite holds. A
/// value that is neither text nor a blob is unreadable too.
pub fn classify(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<StoredTerminal> {
    Ok(match row.get_ref(index)? {
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => StoredTerminal::read(bytes),
        _ => StoredTerminal::Unreadable { call: None },
    })
}

/// Like [`classify`], but `NULL` is no terminal: the column belongs to a
/// row of another kind.
pub fn classify_optional(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<StoredTerminal>> {
    if matches!(row.get_ref(index)?, ValueRef::Null) {
        return Ok(None);
    }
    classify(row, index).map(Some)
}
