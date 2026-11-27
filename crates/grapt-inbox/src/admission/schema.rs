//! Inbox projection schema creation and predecessor migration.

use super::operation_delivery::CREATE_OPERATION_DELIVERY_PENDING;
use super::*;

/// Columns added to predecessor tables after their first shape. The settled
/// check and the migration read the same list.
const MIGRATED_COLUMNS: [(&str, &str, &str); 5] = [
    (
        "session_pending",
        "input_type",
        "TEXT NOT NULL DEFAULT 'user'",
    ),
    ("session_pending", "description", "TEXT"),
    ("session_pending", "metadata_json", "TEXT"),
    ("session_message", "description", "TEXT"),
    ("session_message", "metadata_json", "TEXT"),
];

impl Inbox {
    /// Create the inbox projection tables on a connection. Exposed so another
    /// application service that shares the same database (e.g. subagent
    /// delivery, which admits a typed operation fact on the parent through the inbox
    /// seam) can guarantee the tables exist before composing an admission into
    /// its own transaction.
    pub fn ensure_tables(conn: &rusqlite::Connection) -> Result<(), InboxError> {
        let existing_delivery_table: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'session_operation_delivery_pending'",
            [],
            |row| row.get(0),
        )?;
        let has_settlement: i64 = if existing_delivery_table == 0 {
            0
        } else {
            conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('session_operation_delivery_pending')
                 WHERE name = 'settlement'",
                [],
                |row| row.get(0),
            )?
        };
        if existing_delivery_table == 1 && has_settlement == 0 {
            conn.execute_batch("BEGIN IMMEDIATE")?;
            let migrated = (|| -> Result<(), rusqlite::Error> {
                conn.execute_batch(
                    "ALTER TABLE session_operation_delivery_pending
                     RENAME TO session_operation_delivery_completed_audit;",
                )?;
                conn.execute_batch(CREATE_OPERATION_DELIVERY_PENDING)?;
                conn.execute_batch(
                    "INSERT INTO session_operation_delivery_pending
                         (id, session_id, operation_id, subagent_operation_id, claim_id,
                          generation, settlement, artifact_id, artifact_length, artifact_digest,
                          projection, projection_bytes, admitted_seq)
                     SELECT id, session_id, operation_id, subagent_operation_id, claim_id,
                            generation, 'completed', artifact_id, artifact_length, artifact_digest,
                            projection, projection_bytes, admitted_seq
                     FROM session_operation_delivery_completed_audit;",
                )?;
                Ok(())
            })();
            match migrated {
                Ok(()) => conn.execute_batch("COMMIT")?,
                Err(error) => {
                    let _rollback_result = conn.execute_batch("ROLLBACK");
                    return Err(InboxError::Sqlite(error));
                }
            }
        }
        // Read-only first: a settled schema needs no transaction and takes no
        // write lock at all.
        if Self::current_tables_are_settled(conn)? {
            return Ok(());
        }
        // One transaction for the whole current schema. In autocommit every
        // statement below would commit, and sync, on its own: creating the
        // tables on a fresh database cost a separate durable commit per
        // statement, none of which carries a guarantee the others lack. A
        // caller that already holds a transaction keeps it.
        if !conn.is_autocommit() {
            return Self::ensure_current_tables(conn);
        }
        conn.execute_batch("BEGIN IMMEDIATE")?;
        match Self::ensure_current_tables(conn) {
            Ok(()) => {
                conn.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(error) => {
                let _rollback_result = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Whether `ensure_current_tables` would change nothing: every table and
    /// migrated column exists, and the legacy synthetic-input migration has
    /// nothing left to copy or remove. Read-only. A synthetic event with no
    /// input id is never settled, so such a store keeps taking the write path
    /// exactly as before.
    fn current_tables_are_settled(conn: &rusqlite::Connection) -> Result<bool, InboxError> {
        let tables: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN
                 ('session_pending', 'session_message', 'inbox_legacy_synthetic_audit',
                  'session_operation_delivery_pending')",
            [],
            |row| row.get(0),
        )?;
        if tables != 4 {
            return Ok(false);
        }
        for (table, column, _definition) in MIGRATED_COLUMNS {
            let exists: i64 = conn.query_row(
                &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
                [column],
                |row| row.get(0),
            )?;
            if exists == 0 {
                return Ok(false);
            }
        }
        let unsettled: bool = conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM event
                 WHERE type = 'session.input.admitted.1'
                   AND json_valid(data) = 1
                   AND json_extract(data, '$.input.type') = 'synthetic'
                   AND COALESCE(json_extract(data, '$.input.rustAuthorized'), 0) != 1
                   AND (json_extract(data, '$.inputID') IS NULL
                        OR json_extract(data, '$.inputID')
                           NOT IN (SELECT input_id FROM inbox_legacy_synthetic_audit
                                   WHERE input_id IS NOT NULL)))
             OR EXISTS(
                 SELECT 1 FROM session_pending
                 WHERE id IN (SELECT input_id FROM inbox_legacy_synthetic_audit))",
            [],
            |row| row.get(0),
        )?;
        Ok(!unsettled)
    }

    fn ensure_current_tables(conn: &rusqlite::Connection) -> Result<(), InboxError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS session_pending (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                delivery TEXT NOT NULL,
                text TEXT NOT NULL,
                admitted_seq INTEGER NOT NULL,
                input_type TEXT NOT NULL DEFAULT 'user',
                description TEXT,
                metadata_json TEXT
            );
            CREATE TABLE IF NOT EXISTS session_message (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                type TEXT NOT NULL,
                text TEXT NOT NULL,
                delivery TEXT NOT NULL,
                seq INTEGER NOT NULL,
                description TEXT,
                metadata_json TEXT
            );
            CREATE TABLE IF NOT EXISTS inbox_legacy_synthetic_audit (
                input_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                event_data TEXT NOT NULL
            );",
        )?;
        for (table, column, definition) in MIGRATED_COLUMNS {
            let exists: i64 = conn.query_row(
                &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
                [column],
                |row| row.get(0),
            )?;
            if exists == 0 {
                conn.execute_batch(&format!(
                    "ALTER TABLE {table} ADD COLUMN {column} {definition};"
                ))?;
            }
        }
        conn.execute_batch(CREATE_OPERATION_DELIVERY_PENDING)?;
        // Historical synthetic user rows remain available as audit bytes but
        // are removed from the active pending projection. This migration is
        // idempotent and never rewrites the canonical event.
        conn.execute_batch(
            "INSERT OR IGNORE INTO inbox_legacy_synthetic_audit
                 (input_id, session_id, event_data)
             SELECT json_extract(data, '$.inputID'),
                    json_extract(data, '$.sessionID'), data
             FROM event
             WHERE type = 'session.input.admitted.1'
               AND json_valid(data) = 1
               AND json_extract(data, '$.input.type') = 'synthetic'
               AND COALESCE(json_extract(data, '$.input.rustAuthorized'), 0) != 1;
             DELETE FROM session_pending
             WHERE id IN (SELECT input_id FROM inbox_legacy_synthetic_audit);",
        )?;
        Ok(())
    }
}
