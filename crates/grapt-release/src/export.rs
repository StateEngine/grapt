use grapt_types::DatabaseFingerprint;
use rusqlite::types::ValueRef;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_COLUMNS_PER_TABLE: usize = 256;
const MAX_CANONICAL_CELL_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum DomainExportError {
    #[error("canonical export sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("table {table} exposes {found} columns above the {maximum}-column bound")]
    TooManyColumns {
        table: String,
        maximum: usize,
        found: usize,
    },
    #[error("{table}.{column} contains {found} bytes above the {maximum}-byte cell bound")]
    CellTooLarge {
        table: String,
        column: String,
        maximum: usize,
        found: usize,
    },
    #[error("{table}.{column} contains invalid UTF-8")]
    InvalidUtf8 { table: String, column: String },
    #[error("canonical export row count overflowed u64")]
    RowCountOverflow,
    #[error("canonical export observed a negative SQLite count: {0}")]
    NegativeCount(i64),
    #[error("canonical export fingerprint construction refused")]
    InvalidFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableStateDigest {
    pub table: String,
    pub rows: u64,
    pub digest_hex: String,
    pub absent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainExport {
    pub schema: String,
    pub events: TableStateDigest,
    pub event_sequences: TableStateDigest,
    pub adjunct: Vec<TableStateDigest>,
    pub non_legacy_events: u64,
    pub digest_hex: String,
}

impl DomainExport {
    pub fn fingerprint(&self) -> Result<DatabaseFingerprint, DomainExportError> {
        DatabaseFingerprint::new(self.digest_hex.clone())
            .ok_or(DomainExportError::InvalidFingerprint)
    }
}

pub fn export_domain(connection: &Connection) -> Result<DomainExport, DomainExportError> {
    let event_has_family = column_exists(connection, "event", "aggregate_family")?;
    let sequence_has_family = column_exists(connection, "event_sequence", "aggregate_family")?;
    let events = if event_has_family {
        digest_query(
            connection,
            "event",
            &["id", "aggregate_id", "seq", "created", "type", "data"],
            "SELECT id, aggregate_id, seq, created, type, data FROM event \
             WHERE aggregate_family = 'session' ORDER BY aggregate_id, seq, id",
        )?
    } else {
        digest_query(
            connection,
            "event",
            &["id", "aggregate_id", "seq", "created", "type", "data"],
            "SELECT id, aggregate_id, seq, created, type, data FROM event \
             ORDER BY aggregate_id, seq, id",
        )?
    };
    let event_sequences = if sequence_has_family {
        digest_query(
            connection,
            "event_sequence",
            &["aggregate_id", "seq", "owner_id"],
            "SELECT aggregate_id, seq, owner_id FROM event_sequence \
             WHERE aggregate_family = 'session' ORDER BY aggregate_id",
        )?
    } else {
        digest_query(
            connection,
            "event_sequence",
            &["aggregate_id", "seq", "owner_id"],
            "SELECT aggregate_id, seq, owner_id FROM event_sequence ORDER BY aggregate_id",
        )?
    };
    let non_legacy_events = if event_has_family {
        let count = connection.query_row(
            "SELECT COUNT(*) FROM event WHERE aggregate_family <> 'session'",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        u64::try_from(count).map_err(|_| DomainExportError::NegativeCount(count))?
    } else {
        0
    };

    let mut adjunct = Vec::new();
    for table in grapt_store::export::EVENT_REPLAYABLE_TABLES
        .iter()
        .chain(grapt_store::export::MUTABLE_ONLY_TABLES.iter())
    {
        adjunct.push(digest_entire_table(connection, table)?);
    }

    let mut hasher = Sha256::new();
    hash_table_state(&mut hasher, &events);
    hash_table_state(&mut hasher, &event_sequences);
    hasher.update(non_legacy_events.to_le_bytes());
    for table in &adjunct {
        hash_table_state(&mut hasher, table);
    }
    Ok(DomainExport {
        schema: "grapt.release.domain-export.v1".to_owned(),
        events,
        event_sequences,
        adjunct,
        non_legacy_events,
        digest_hex: format!("{:x}", hasher.finalize()),
    })
}

fn column_exists(
    connection: &Connection,
    table: &str,
    column: &str,
) -> Result<bool, DomainExportError> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool, rusqlite::Error> {
    connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count != 0)
}

fn digest_entire_table(
    connection: &Connection,
    table: &str,
) -> Result<TableStateDigest, DomainExportError> {
    if !table_exists(connection, table)? {
        return Ok(absent_digest(table));
    }
    let mut pragma = connection.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let columns = pragma
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if columns.len() > MAX_COLUMNS_PER_TABLE {
        return Err(DomainExportError::TooManyColumns {
            table: table.to_owned(),
            maximum: MAX_COLUMNS_PER_TABLE,
            found: columns.len(),
        });
    }
    let rendered = columns
        .iter()
        .map(|column| format!("\"{}\"", column.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!("SELECT {rendered} FROM \"{table}\" ORDER BY {rendered}");
    digest_query(connection, table, &columns, &query)
}

fn digest_query(
    connection: &Connection,
    table: &str,
    columns: &[impl AsRef<str>],
    sql: &str,
) -> Result<TableStateDigest, DomainExportError> {
    if !table_exists(connection, table)? {
        return Ok(absent_digest(table));
    }
    if columns.len() > MAX_COLUMNS_PER_TABLE {
        return Err(DomainExportError::TooManyColumns {
            table: table.to_owned(),
            maximum: MAX_COLUMNS_PER_TABLE,
            found: columns.len(),
        });
    }
    let mut statement = connection.prepare(sql)?;
    let mut rows = statement.query([])?;
    let mut hasher = Sha256::new();
    let mut count = 0_u64;
    while let Some(row) = rows.next()? {
        for (index, column) in columns.iter().enumerate() {
            hash_value(&mut hasher, row.get_ref(index)?, table, column.as_ref())?;
        }
        hasher.update([0xff]);
        count = count
            .checked_add(1)
            .ok_or(DomainExportError::RowCountOverflow)?;
    }
    Ok(TableStateDigest {
        table: table.to_owned(),
        rows: count,
        digest_hex: format!("{:x}", hasher.finalize()),
        absent: false,
    })
}

fn hash_value(
    hasher: &mut Sha256,
    value: ValueRef<'_>,
    table: &str,
    column: &str,
) -> Result<(), DomainExportError> {
    match value {
        ValueRef::Null => hasher.update(b"z"),
        ValueRef::Integer(value) => {
            hasher.update(b"i");
            hasher.update(value.to_le_bytes());
        }
        ValueRef::Real(value) => {
            hasher.update(b"r");
            hasher.update(value.to_bits().to_le_bytes());
        }
        ValueRef::Text(bytes) => {
            bounded_cell(table, column, bytes.len())?;
            core::str::from_utf8(bytes).map_err(|_| DomainExportError::InvalidUtf8 {
                table: table.to_owned(),
                column: column.to_owned(),
            })?;
            hasher.update(b"t");
            hash_bytes(hasher, bytes);
        }
        ValueRef::Blob(bytes) => {
            bounded_cell(table, column, bytes.len())?;
            hasher.update(b"b");
            hash_bytes(hasher, bytes);
        }
    }
    Ok(())
}

fn bounded_cell(table: &str, column: &str, found: usize) -> Result<(), DomainExportError> {
    if found > MAX_CANONICAL_CELL_BYTES {
        return Err(DomainExportError::CellTooLarge {
            table: table.to_owned(),
            column: column.to_owned(),
            maximum: MAX_CANONICAL_CELL_BYTES,
            found,
        });
    }
    Ok(())
}

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn absent_digest(table: &str) -> TableStateDigest {
    TableStateDigest {
        table: table.to_owned(),
        rows: 0,
        digest_hex: String::new(),
        absent: true,
    }
}

fn hash_table_state(hasher: &mut Sha256, table: &TableStateDigest) {
    hash_bytes(hasher, table.table.as_bytes());
    hasher.update(table.rows.to_le_bytes());
    hasher.update([u8::from(table.absent)]);
    hash_bytes(hasher, table.digest_hex.as_bytes());
}
