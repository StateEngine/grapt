//! SC-REL-04: WAL readers do not block the writer, and NORMAL avoids a sync
//! on every tiny commit. Transactions still end at each durable effect boundary.
//! Copyright 2025 OpenAI. Apache-2.0; see licenses/openai-codex/
use super::{Connection, WriteError};

pub(super) fn configure(conn: &Connection) -> Result<(), WriteError> {
    let file: String = conn.query_row(
        "SELECT file FROM pragma_database_list WHERE name = 'main'",
        [],
        |row| row.get(0),
    )?;
    let journal: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    let synchronous: i64 = conn.pragma_query_value(None, "synchronous", |row| row.get(0))?;
    let expected = if file.is_empty() { "memory" } else { "wal" };
    if journal != expected || synchronous != 1 {
        return Err(WriteError::JournalUnavailable {
            journal,
            synchronous,
        });
    }
    Ok(())
}
