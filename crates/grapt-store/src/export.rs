//! Canonical exporter SKELETON over BOTH state classes (red-team §F.3; D1
//! coverage list). The exporter is the cross-engine differential surface and
//! the migration-safety net for state that replay cannot reconstruct.
//!
//! Two classes, from the D1 evidence report (measured on the frozen clone):
//! - event-replayable: derivable from `event` replay (session, session_message,
//!   session_pending, instruction_*, permission_request, permission_http_*,
//!   form_request);
//! - mutable-only PRIMARY state with NO durable log — replay can NEVER rebuild
//!   these; any migration must capture them independently.
//!
//! Skeleton scope: canonical per-table digests + row counts, deterministic
//! by primary-key order. Domain-shaped export (typed rows instead of raw
//! column maps) lands with each owning domain crate; the class split and the
//! digest law are the contract this skeleton fixes.

use rusqlite::Connection;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// Mutable-only tables (no durable log behind them) — D1 §5 plus the plugin
/// credential ownership state admitted for plugins.
pub const MUTABLE_ONLY_TABLES: &[&str] = &[
    "subagent_operation",
    "permission",
    "kv",
    "credential",
    "plugin_credential_owner",
    "project",
    "project_directory",
    "workspace",
    "account",
    "account_state",
    "control_account",
    "effective_graph",
    "session_share",
    "data_migration",
    "message",
    "part",
];

/// Event-replayable projection tables — D1 §5, verbatim.
pub const EVENT_REPLAYABLE_TABLES: &[&str] = &[
    "session",
    "session_message",
    "session_pending",
    "instruction_entry",
    "instruction_blob",
    "instruction_state",
    "permission_request",
    "permission_http_rule",
    "permission_http_request",
    "permission_http_rule_revocation",
    "permission_http_rule_origin",
    "form_request",
];

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("{table}.{column}: REAL value {value} is not representable as JSON")]
    NonFiniteReal {
        table: String,
        column: String,
        value: f64,
    },
    #[error("{table}.{column}: TEXT is not valid UTF-8")]
    InvalidUtf8 { table: String, column: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDigest {
    pub table: String,
    pub rows: u64,
    /// SHA-256 over the canonical JSON of every row (ordered by the table's
    /// full column tuple — deterministic and engine-neutral, no rowid).
    pub digest_hex: String,
    /// True when the table is absent in this database (older schema era).
    pub absent: bool,
}

#[derive(Debug)]
pub struct CanonicalExport {
    pub event_replayable: Vec<TableDigest>,
    pub mutable_only: Vec<TableDigest>,
    pub event_log: TableDigest,
}

/// Fail-closed row conversion: a value the canonical form cannot represent
/// exactly is an error, never a silent Null/lossy substitute (silent
/// substitution creates digest collisions between distinct databases).
fn row_to_canonical(
    row: &rusqlite::Row<'_>,
    columns: &[String],
    table: &str,
) -> Result<Value, ExportError> {
    // Every cell is tagged with its SQLite storage class so distinct classes
    // can never share a canonical form: TEXT "ab" is ["t","ab"], BLOB x'AB'
    // is ["b","ab"], INTEGER 1 is ["i",1], REAL 1.0 is ["r",1.0], NULL is
    // ["z"] (review round 2, item D).
    let mut map = Map::new();
    for (i, name) in columns.iter().enumerate() {
        let v = match row.get_ref(i)? {
            rusqlite::types::ValueRef::Null => Value::Array(vec![Value::from("z")]),
            rusqlite::types::ValueRef::Integer(n) => {
                Value::Array(vec![Value::from("i"), Value::from(n)])
            }
            rusqlite::types::ValueRef::Real(f) => Value::Array(vec![
                Value::from("r"),
                serde_json::Number::from_f64(f)
                    .map(Value::Number)
                    .ok_or_else(|| ExportError::NonFiniteReal {
                        table: table.to_owned(),
                        column: name.clone(),
                        value: f,
                    })?,
            ]),
            rusqlite::types::ValueRef::Text(t) => Value::Array(vec![
                Value::from("t"),
                Value::from(
                    std::str::from_utf8(t)
                        .map_err(|_| ExportError::InvalidUtf8 {
                            table: table.to_owned(),
                            column: name.clone(),
                        })?
                        .to_owned(),
                ),
            ]),
            rusqlite::types::ValueRef::Blob(b) => Value::Array(vec![
                Value::from("b"),
                Value::from(b.iter().map(|x| format!("{x:02x}")).collect::<String>()),
            ]),
        };
        map.insert(name.clone(), v);
    }
    Ok(Value::Object(map))
}

fn digest_table(conn: &Connection, table: &str) -> Result<TableDigest, ExportError> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |r| r.get(0),
    )?;
    if exists == 0 {
        return Ok(TableDigest {
            table: table.to_owned(),
            rows: 0,
            digest_hex: String::new(),
            absent: true,
        });
    }
    let columns: Vec<String> = {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
        let cols = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        cols
    };
    let order = columns
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = conn.prepare(&format!("SELECT * FROM \"{table}\" ORDER BY {order}"))?;
    let mut rows = stmt.query([])?;
    let mut hasher = Sha256::new();
    let mut count = 0u64;
    while let Some(row) = rows.next()? {
        hasher.update(
            grapt_event::canonical_string(&row_to_canonical(row, &columns, table)?).as_bytes(),
        );
        hasher.update(
            b"
",
        );
        count += 1;
    }
    let digest_hex = format!("{:x}", hasher.finalize());
    Ok(TableDigest {
        table: table.to_owned(),
        rows: count,
        digest_hex,
        absent: false,
    })
}

/// Export canonical digests for both state classes plus the event log itself.
pub fn export_canonical(conn: &Connection) -> Result<CanonicalExport, ExportError> {
    let mut event_replayable = Vec::new();
    for t in EVENT_REPLAYABLE_TABLES {
        event_replayable.push(digest_table(conn, t)?);
    }
    let mut mutable_only = Vec::new();
    for t in MUTABLE_ONLY_TABLES {
        mutable_only.push(digest_table(conn, t)?);
    }
    let event_log = digest_table(conn, "event")?;
    Ok(CanonicalExport {
        event_replayable,
        mutable_only,
        event_log,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_deterministic_and_absence_is_reported() -> Result<(), Box<dyn std::error::Error>> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO kv VALUES ('b', '2'), ('a', '1');",
        )?;
        let one = digest_table(&conn, "kv")?;
        let two = digest_table(&conn, "kv")?;
        assert_eq!(one, two);
        assert_eq!(one.rows, 2);
        assert!(!one.digest_hex.is_empty());
        let missing = digest_table(&conn, "credential")?;
        assert!(missing.absent);
        Ok(())
    }

    #[test]
    fn storage_classes_never_collide() -> Result<(), Box<dyn std::error::Error>> {
        // TEXT 'ab' vs BLOB x'AB': identical bytes, different classes.
        let text_db = Connection::open_in_memory()?;
        text_db.execute_batch("CREATE TABLE c (v); INSERT INTO c VALUES ('ab')")?;
        let blob_db = Connection::open_in_memory()?;
        blob_db.execute_batch("CREATE TABLE c (v); INSERT INTO c VALUES (x'AB')")?;
        let text_digest = digest_table(&text_db, "c")?;
        let blob_digest = digest_table(&blob_db, "c")?;
        assert_ne!(
            text_digest.digest_hex, blob_digest.digest_hex,
            "TEXT and BLOB with identical bytes must not share a digest"
        );

        // INTEGER 1 vs REAL 1.0: numerically equal, different classes.
        let int_db = Connection::open_in_memory()?;
        int_db.execute_batch("CREATE TABLE c (v); INSERT INTO c VALUES (1)")?;
        let real_db = Connection::open_in_memory()?;
        real_db.execute_batch("CREATE TABLE c (v); INSERT INTO c VALUES (1.0)")?;
        let int_digest = digest_table(&int_db, "c")?;
        let real_digest = digest_table(&real_db, "c")?;
        assert_ne!(
            int_digest.digest_hex, real_digest.digest_hex,
            "INTEGER and REAL with equal values must not share a digest"
        );
        Ok(())
    }
}
