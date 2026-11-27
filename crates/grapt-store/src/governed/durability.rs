//! Select durability before BEGIN; COMMIT has no fallible post-commit work.
use rusqlite::Connection;

pub(super) fn prepare(conn: &Connection, durable: bool) -> rusqlite::Result<()> {
    conn.pragma_update(None, "synchronous", if durable { "FULL" } else { "NORMAL" })
}

pub(super) fn require(durable: bool) -> rusqlite::Result<()> {
    if durable {
        Ok(())
    } else {
        Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISUSE),
            Some("a NORMAL stream transaction cannot record an effect intent".into()),
        ))
    }
}

pub(super) fn require_connection(conn: &Connection) -> rusqlite::Result<()> {
    let synchronous: i64 = conn.pragma_query_value(None, "synchronous", |row| row.get(0))?;
    require(synchronous >= 2)
}
