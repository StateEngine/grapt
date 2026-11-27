use super::InstructionError;
use crate::{classify_blob, BlobAddressing};
use grapt_types::{EventSeq, InstructionEpoch, InstructionHash, InstructionKey, SessionId};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension, Transaction};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug)]
pub(super) struct StoredEntry {
    pub(super) value: Option<String>,
    pub(super) removed: bool,
    pub(super) created_ms: i64,
    pub(super) updated_ms: i64,
}

/// Bound persisted data before any entry value is allocated. Retired keys
/// count too: churn cannot grow an unbounded per-session tombstone census.
pub(super) fn entry_bounds(
    conn: &Connection,
    session: &SessionId,
    replacement: Option<(&InstructionKey, usize)>,
) -> Result<(), InstructionError> {
    let mut statement = conn.prepare(
        "SELECT length(CAST(key AS BLOB)), length(CAST(value AS BLOB)), key = ?2
         FROM instruction_entry WHERE session_id = ?1 LIMIT 257",
    )?;
    let mut rows = statement.query(rusqlite::params![
        session.as_str(),
        replacement.map(|(key, _)| key.as_str())
    ])?;
    let mut count = 0usize;
    let mut bytes = 0usize;
    let mut replacing = false;
    while let Some(row) = rows.next()? {
        count += 1;
        if count > 256 {
            return Err(InstructionError::EntryBound {
                bound: "256 entry keys per session",
            });
        }
        let key: i64 = row.get(0)?;
        let value: Option<i64> = row.get(1)?;
        if !(1..=256).contains(&key) || value.is_some_and(|size| !(0..=65_536).contains(&size)) {
            return Err(InstructionError::EntryBound {
                bound: "persisted key/value byte limits",
            });
        }
        let key = usize::try_from(key).map_err(|_| InstructionError::EntryBound {
            bound: "key byte conversion",
        })?;
        let value =
            usize::try_from(value.unwrap_or(0)).map_err(|_| InstructionError::EntryBound {
                bound: "value byte conversion",
            })?;
        let matches: Option<bool> = row.get(2)?;
        replacing |= matches == Some(true);
        bytes += key + if matches == Some(true) { 0 } else { value };
    }
    if let Some((key, value)) = replacement {
        if !replacing {
            if count == 256 {
                return Err(InstructionError::EntryBound {
                    bound: "256 entry keys per session",
                });
            }
            bytes += key.as_str().len();
        }
        bytes += value;
    }
    if bytes > 4_194_304 {
        return Err(InstructionError::EntryBound {
            bound: "4 MiB entry bytes per session",
        });
    }
    Ok(())
}

pub(super) fn ensure_tables(tx: &Transaction<'_>) -> Result<(), InstructionError> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS instruction_blob (
            hash TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS instruction_entry (
            session_id TEXT NOT NULL,
            key TEXT NOT NULL,
            value TEXT,
            removed INTEGER NOT NULL DEFAULT 0 CHECK (removed IN (0, 1)),
            time_created INTEGER NOT NULL,
            time_updated INTEGER NOT NULL,
            PRIMARY KEY (session_id, key),
            CHECK ((removed = 0 AND value IS NOT NULL) OR (removed = 1 AND value IS NULL))
        );
        CREATE TABLE IF NOT EXISTS instruction_state (
            session_id TEXT PRIMARY KEY,
            epoch_start INTEGER NOT NULL CHECK (epoch_start >= 0),
            through_seq INTEGER NOT NULL CHECK (through_seq >= epoch_start),
            initial_values TEXT NOT NULL,
            current_values TEXT NOT NULL
        );",
    )?;
    verify_schema(tx)
}

pub(super) fn schema_is_current(conn: &Connection) -> Result<bool, InstructionError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'
         AND name IN ('instruction_blob', 'instruction_entry', 'instruction_state')",
        [],
        |row| row.get(0),
    )?;
    if count != 3 {
        return Ok(false);
    }
    verify_schema(conn)?;
    Ok(true)
}

fn verify_schema(conn: &Connection) -> Result<(), InstructionError> {
    verify_columns(conn, "instruction_blob", &["hash", "value"])?;
    verify_columns(
        conn,
        "instruction_entry",
        &[
            "session_id",
            "key",
            "value",
            "removed",
            "time_created",
            "time_updated",
        ],
    )?;
    verify_columns(
        conn,
        "instruction_state",
        &[
            "session_id",
            "epoch_start",
            "through_seq",
            "initial_values",
            "current_values",
        ],
    )
}

fn verify_columns(
    conn: &Connection,
    table: &str,
    required: &[&str],
) -> Result<(), InstructionError> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let present = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    let missing = required
        .iter()
        .filter(|column| !present.iter().any(|found| found == **column))
        .copied()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(InstructionError::Corruption {
        reason: format!("table {table} is missing columns {missing:?}"),
    })
}

pub(super) fn insert_blob(
    tx: &Transaction<'_>,
    hash: InstructionHash,
    value: &str,
) -> Result<(), InstructionError> {
    tx.execute(
        "INSERT INTO instruction_blob (hash, value) VALUES (?1, ?2)
         ON CONFLICT(hash) DO NOTHING",
        rusqlite::params![hash.to_string(), value],
    )?;
    let stored: Option<String> = tx
        .query_row(
            "SELECT value FROM instruction_blob WHERE hash = ?1",
            [hash.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    let stored = stored.ok_or_else(|| InstructionError::Corruption {
        reason: format!("blob {} disappeared during insertion", hash),
    })?;
    if stored != value || classify_blob(hash, &stored) == BlobAddressing::Violation {
        return Err(InstructionError::Corruption {
            reason: format!("blob {} does not match its content address", hash),
        });
    }
    Ok(())
}

pub(super) fn instruction_event_data(
    session: &SessionId,
    delta: &BTreeMap<InstructionKey, Option<InstructionHash>>,
) -> Value {
    let values = delta
        .iter()
        .map(|(key, hash)| {
            (
                key.to_string(),
                Value::String(hash.map_or_else(|| "removed".into(), |hash| hash.to_string())),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    serde_json::json!({"sessionID": session.as_str(), "delta": values})
}

fn encode_hash_map(values: &BTreeMap<InstructionKey, InstructionHash>) -> String {
    grapt_event::canonical_string(&Value::Object(
        values
            .iter()
            .map(|(key, hash)| (key.to_string(), Value::String(hash.to_string())))
            .collect(),
    ))
}

pub(super) fn write_state(
    tx: &Transaction<'_>,
    session: &SessionId,
    epoch: InstructionEpoch,
    through: EventSeq,
    initial: &BTreeMap<InstructionKey, InstructionHash>,
    current: &BTreeMap<InstructionKey, InstructionHash>,
) -> Result<(), InstructionError> {
    let epoch = i64::try_from(epoch.value()).map_err(|_| InstructionError::Corruption {
        reason: "instruction epoch exceeds sqlite range".into(),
    })?;
    let through = i64::try_from(through.value()).map_err(|_| InstructionError::Corruption {
        reason: "instruction sequence exceeds sqlite range".into(),
    })?;
    tx.execute(
        "INSERT INTO instruction_state
            (session_id, epoch_start, through_seq, initial_values, current_values)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(session_id) DO UPDATE SET
            epoch_start = excluded.epoch_start,
            through_seq = excluded.through_seq,
            initial_values = excluded.initial_values,
            current_values = excluded.current_values",
        rusqlite::params![
            session.as_str(),
            epoch,
            through,
            encode_hash_map(initial),
            encode_hash_map(current)
        ],
    )?;
    Ok(())
}

/// The blob `hash` names. A blob that is absent, not text, or not the content
/// its address names is [`InstructionError::Corruption`]: damage to the rows
/// that reference it, which a fold may skip. SQLite's own failures stay
/// [`InstructionError::Sqlite`].
pub(super) fn load_blob(
    conn: &Connection,
    hash: InstructionHash,
) -> Result<String, InstructionError> {
    let value = conn
        .query_row(
            "SELECT value FROM instruction_blob WHERE hash = ?1",
            [hash.to_string()],
            |row| {
                Ok(match row.get_ref(0)? {
                    ValueRef::Text(text) => std::str::from_utf8(text)
                        .map(str::to_owned)
                        .map_err(|error| error.to_string()),
                    other => Err(format!("is {}, not text", other.data_type())),
                })
            },
        )
        .optional()?
        .ok_or_else(|| InstructionError::Corruption {
            reason: format!("instruction hash {hash} has no blob"),
        })?
        .map_err(|reason| InstructionError::Corruption {
            reason: format!("instruction blob {hash} {reason}"),
        })?;
    if classify_blob(hash, &value) == BlobAddressing::Violation {
        return Err(InstructionError::Corruption {
            reason: format!("instruction blob {hash} violates its content address"),
        });
    }
    Ok(value)
}

pub(super) fn load_entry(
    conn: &Connection,
    session: &SessionId,
    key: &InstructionKey,
) -> Result<Option<StoredEntry>, InstructionError> {
    conn.query_row(
        "SELECT value, removed, time_created, time_updated
         FROM instruction_entry WHERE session_id = ?1 AND key = ?2",
        rusqlite::params![session.as_str(), key.as_str()],
        |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        },
    )
    .optional()?
    .map(|(value, removed, created, updated)| decode_entry(value, removed, created, updated))
    .transpose()
}

pub(super) fn decode_entry(
    value: Option<String>,
    removed: i64,
    created_ms: i64,
    updated_ms: i64,
) -> Result<StoredEntry, InstructionError> {
    if updated_ms < created_ms {
        return Err(InstructionError::Corruption {
            reason: "instruction entry update precedes creation".into(),
        });
    }
    match (removed, value) {
        (0, Some(value)) => Ok(StoredEntry {
            value: Some(value),
            removed: false,
            created_ms,
            updated_ms,
        }),
        (1, None) => Ok(StoredEntry {
            value: None,
            removed: true,
            created_ms,
            updated_ms,
        }),
        _ => Err(InstructionError::Corruption {
            reason: "instruction entry has an invalid removed/value combination".into(),
        }),
    }
}
