//! Engine-epoch stamping — the ratified B.1 mechanism. The FIRST Rust write contact with
//! a database stamps ownership in the SAME immediate transaction as the schema it
//! prepares, so a partial failure can never leave Rust ownership behind a missing schema.
//!
//! `PRAGMA user_version` is the primary marker; the `engine_epoch` row adds
//! attribution. oracle-era databases carry `user_version = 0` (verified on the
//! frozen clone). A database claiming the Rust epoch with missing, duplicate,
//! or wrong attribution is CORRUPT and refuses — there is no repair path
//! (review item 2).

use grapt_types::EngineEpoch;
use rusqlite::{Connection, TransactionBehavior};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum EpochError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database is owned by a foreign/newer engine epoch {found}; refusing to write")]
    ForeignEpoch { found: u32 },
    #[error("epoch attribution corrupt: {reason}; refusing to write")]
    CorruptAttribution { reason: String },
}

#[derive(Debug, PartialEq, Eq)]
pub enum EpochOutcome {
    /// oracle-era database: this call stamped it as Rust-owned.
    Stamped,
    /// Already Rust-owned with exactly one valid attribution row.
    AlreadyRust,
}

/// Read-only epoch validation: no mutation on any path. Distinguishes a
/// oracle-era database (Ok(None)) from a valid Rust-owned one (Ok(Some)) and
/// refuses foreign or corrupt states.
pub fn validate_read_only(conn: &Connection) -> Result<Option<EpochOutcome>, EpochError> {
    let found: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if found == EngineEpoch::TYPESCRIPT.0 {
        return Ok(None);
    }
    if found != EngineEpoch::RUST.0 {
        return Err(EpochError::ForeignEpoch { found });
    }
    let table: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'engine_epoch'",
        [],
        |r| r.get(0),
    )?;
    if table == 0 {
        return Err(EpochError::CorruptAttribution {
            reason: "user_version claims the Rust epoch but engine_epoch is missing".into(),
        });
    }
    let rows: i64 = conn.query_row("SELECT COUNT(*) FROM engine_epoch", [], |r| r.get(0))?;
    if rows != 1 {
        return Err(EpochError::CorruptAttribution {
            reason: format!("expected exactly one attribution row, found {rows}"),
        });
    }
    let (epoch, engine): (u32, String) =
        conn.query_row("SELECT epoch, engine FROM engine_epoch", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    if epoch != EngineEpoch::RUST.0 || engine != "secode-rust" {
        return Err(EpochError::CorruptAttribution {
            reason: format!("attribution row is epoch {epoch} engine {engine:?}"),
        });
    }
    Ok(Some(EpochOutcome::AlreadyRust))
}

/// Read-only attribution version: the build string stamped at the first Rust
/// write contact, or `None` for an oracle-era database. No mutation on any path.
/// Only the half-published-profile completion ownership check consumes this;
/// ordinary profile opens never read the version string, because updates
/// legitimately open stores stamped by older builds.
pub fn attribution_version_read_only(conn: &Connection) -> Result<Option<String>, EpochError> {
    let found: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if found == EngineEpoch::TYPESCRIPT.0 {
        return Ok(None);
    }
    if found != EngineEpoch::RUST.0 {
        return Err(EpochError::ForeignEpoch { found });
    }
    validate_read_only(conn)?;
    let version: String = conn.query_row("SELECT version FROM engine_epoch", [], |r| r.get(0))?;
    Ok(Some(version))
}

/// The attribution read of a database file, through a read-only connection:
/// no mutation on any path. A hot rollback journal refuses every read-only
/// page read; that is reported as [`AttributionRead::HotJournal`], not as
/// corruption — ownership cannot be verified while it exists, so the caller
/// must either replay it under an owned open (a mutation) or refuse the
/// profile without modifying it.
#[derive(Debug, PartialEq)]
pub enum AttributionRead {
    /// oracle-era database: no attribution stamped yet.
    TsEra,
    /// A valid Rust-owned attribution: the stamped build string.
    Version(String),
    /// A hot rollback journal refused the read-only read.
    HotJournal,
}

#[derive(Debug, thiserror::Error)]
pub enum AttributionError {
    #[error("database is owned by a foreign/newer engine epoch {found}; refusing")]
    ForeignEpoch { found: u32 },
    #[error("epoch attribution corrupt: {reason}; refusing")]
    Corrupt { reason: String },
    #[error("attribution unreadable: {0}")]
    Unreadable(rusqlite::Error),
}

pub fn attribution_version_at(path: &Path) -> Result<AttributionRead, AttributionError> {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(AttributionError::Unreadable)?;
    let found: Result<u32, rusqlite::Error> =
        conn.query_row("PRAGMA user_version", [], |r| r.get(0));
    let found = match found {
        Ok(found) => found,
        Err(error) if is_hot_journal_error(&error) => return Ok(AttributionRead::HotJournal),
        Err(error) => return Err(AttributionError::Unreadable(error)),
    };
    if found == EngineEpoch::TYPESCRIPT.0 {
        return Ok(AttributionRead::TsEra);
    }
    if found != EngineEpoch::RUST.0 {
        return Err(AttributionError::ForeignEpoch { found });
    }
    validate_read_only(&conn).map_err(|error| match error {
        EpochError::CorruptAttribution { reason } => AttributionError::Corrupt { reason },
        other => AttributionError::Corrupt {
            reason: other.to_string(),
        },
    })?;
    let version: String = conn
        .query_row("SELECT version FROM engine_epoch", [], |r| r.get(0))
        .map_err(|_| AttributionError::Corrupt {
            reason: "attribution version unreadable".into(),
        })?;
    Ok(AttributionRead::Version(version))
}

fn is_hot_journal_error(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.extended_code == rusqlite::ffi::SQLITE_READONLY_ROLLBACK
    )
}

/// Validate the epoch, stamp an oracle-era database, and run `prepare` (schema
/// creation), ALL in one immediate transaction. Any failure — including a
/// failing `prepare` — leaves the database byte-for-byte unowned: no
/// user_version change, no attribution table.
///
/// `prepare` returns how many row changes it made and rolled back again
/// itself (a probe's savepoint rows). When the database was already Rust and
/// the preparation changed nothing durable (no schema change, and no row
/// change beyond those rolled back), the transaction ends in ROLLBACK: its
/// durable effect is empty either way, and a COMMIT of the probe's journaled
/// pages would cost a full journal and database sync on every open.
pub fn stamp_and_prepare(
    conn: &mut Connection,
    engine_version: &str,
    prepare: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<u64, rusqlite::Error>,
) -> Result<EpochOutcome, EpochError> {
    // BEGIN IMMEDIATE FIRST, then validate ON the transaction: two concurrent
    // first-openers serialize here, and the loser re-observes the winner's
    // stamp instead of racing it (review round 2, item A).
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let outcome = match validate_read_only(&tx)? {
        Some(_) => EpochOutcome::AlreadyRust,
        None => {
            stamp_within(&tx, engine_version)?;
            EpochOutcome::Stamped
        }
    };
    let schema_before = schema_version(&tx)?;
    let changes_before = tx.total_changes();
    let rolled_back = prepare(&tx)?;
    let persisted = tx.total_changes() - changes_before - rolled_back;
    if outcome == EpochOutcome::AlreadyRust
        && schema_version(&tx)? == schema_before
        && persisted == 0
    {
        tx.rollback()?;
    } else {
        tx.commit()?;
    }
    Ok(outcome)
}

fn schema_version(tx: &rusqlite::Transaction<'_>) -> Result<i64, rusqlite::Error> {
    tx.query_row("PRAGMA schema_version", [], |row| row.get(0))
}

/// Stamp inside an existing transaction (shared with the migration path so
/// its first transaction can atomically stamp + create journal + migrate).
pub(crate) fn stamp_within(
    tx: &rusqlite::Transaction<'_>,
    engine_version: &str,
) -> Result<(), rusqlite::Error> {
    tx.execute_batch(&format!("PRAGMA user_version = {}", EngineEpoch::RUST.0))?;
    tx.execute(
        "CREATE TABLE engine_epoch (
            epoch INTEGER NOT NULL,
            engine TEXT NOT NULL,
            version TEXT NOT NULL,
            time INTEGER NOT NULL
        )",
        [],
    )?;
    tx.execute(
        "INSERT INTO engine_epoch (epoch, engine, version, time)
         VALUES (?1, 'secode-rust', ?2, strftime('%s','now') * 1000)",
        rusqlite::params![EngineEpoch::RUST.0, engine_version],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_types::IdAuthority;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn fresh() -> Result<Connection, Box<dyn std::error::Error>> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("CREATE TABLE session (id TEXT PRIMARY KEY)")?;
        Ok(conn)
    }

    #[test]
    fn stamps_once_then_recognizes_ownership() -> TestResult {
        let mut conn = fresh()?;
        assert_eq!(
            stamp_and_prepare(&mut conn, "0.0.1", |_| Ok(0))?,
            EpochOutcome::Stamped
        );
        assert_eq!(
            stamp_and_prepare(&mut conn, "0.0.1", |_| Ok(0))?,
            EpochOutcome::AlreadyRust
        );
        let rows: i64 = conn.query_row("SELECT COUNT(*) FROM engine_epoch", [], |r| r.get(0))?;
        assert_eq!(rows, 1);
        Ok(())
    }

    #[test]
    fn failing_prepare_leaves_the_database_unowned() -> TestResult {
        let mut conn = fresh()?;
        let out = stamp_and_prepare(&mut conn, "0.0.1", |tx| {
            // Simulates an incompatible preexisting schema: preparation fails.
            tx.execute_batch("CREATE TABLE session (id TEXT PRIMARY KEY)")
                .map(|()| 0)
        });
        assert!(out.is_err(), "prepare failure must fail the open");
        let v: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        assert_eq!(v, 0, "no Rust ownership left behind");
        let attribution: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'engine_epoch'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(attribution, 0, "no attribution table left behind");
        Ok(())
    }

    #[test]
    fn concurrent_first_openers_serialize_to_one_stamp() -> TestResult {
        let dir = std::env::temp_dir().join(format!("grapt-epoch-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let db = dir.join("race.db");
        {
            let conn = Connection::open(&db)?;
            conn.execute_batch("CREATE TABLE session (id TEXT PRIMARY KEY)")?;
        }
        let mut handles = Vec::new();
        for _ in 0..2 {
            let path = db.clone();
            handles.push(std::thread::spawn(
                move || -> Result<EpochOutcome, String> {
                    let mut conn = Connection::open(&path).map_err(|e| e.to_string())?;
                    conn.busy_timeout(std::time::Duration::from_secs(10))
                        .map_err(|e| e.to_string())?;
                    stamp_and_prepare(&mut conn, "0.0.1", |_| Ok(0)).map_err(|e| e.to_string())
                },
            ));
        }
        let mut outcomes = Vec::new();
        for h in handles {
            match h.join() {
                Ok(Ok(outcome)) => outcomes.push(outcome),
                Ok(Err(e)) => panic!("concurrent opener failed: {e}"),
                Err(_) => panic!("opener thread panicked"),
            }
        }
        outcomes.sort_by_key(|o| matches!(o, EpochOutcome::AlreadyRust));
        assert_eq!(
            outcomes,
            vec![EpochOutcome::Stamped, EpochOutcome::AlreadyRust],
            "exactly one stamper; the loser deterministically observes AlreadyRust"
        );
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn refuses_foreign_and_corrupt_epochs_without_touching_them() -> TestResult {
        let mut conn = fresh()?;
        conn.execute_batch("PRAGMA user_version = 7")?;
        assert!(matches!(
            stamp_and_prepare(&mut conn, "0.0.1", |_| Ok(0)),
            Err(EpochError::ForeignEpoch { found: 7 })
        ));

        // Rust epoch claimed, attribution missing entirely.
        let mut missing = fresh()?;
        missing.execute_batch("PRAGMA user_version = 2")?;
        assert!(matches!(
            stamp_and_prepare(&mut missing, "0.0.1", |_| Ok(0)),
            Err(EpochError::CorruptAttribution { .. })
        ));

        // Duplicate attribution rows.
        let mut dup = fresh()?;
        dup.execute_batch(
            "PRAGMA user_version = 2;
             CREATE TABLE engine_epoch (epoch INTEGER NOT NULL, engine TEXT NOT NULL, version TEXT NOT NULL, time INTEGER NOT NULL);
             INSERT INTO engine_epoch VALUES (2, 'secode-rust', 'a', 0), (2, 'secode-rust', 'b', 0);",
        )?;
        assert!(matches!(
            stamp_and_prepare(&mut dup, "0.0.1", |_| Ok(0)),
            Err(EpochError::CorruptAttribution { .. })
        ));

        // Wrong engine attribution.
        let mut wrong = fresh()?;
        wrong.execute_batch(
            "PRAGMA user_version = 2;
             CREATE TABLE engine_epoch (epoch INTEGER NOT NULL, engine TEXT NOT NULL, version TEXT NOT NULL, time INTEGER NOT NULL);
             INSERT INTO engine_epoch VALUES (2, 'someone-else', 'x', 0);",
        )?;
        let err = stamp_and_prepare(&mut wrong, "0.0.1", |_| Ok(0));
        assert!(matches!(err, Err(EpochError::CorruptAttribution { .. })));
        // Refusal never repaired or mutated the corrupt state.
        let rows: i64 = wrong.query_row("SELECT COUNT(*) FROM engine_epoch", [], |r| r.get(0))?;
        assert_eq!(rows, 1);
        Ok(())
    }

    #[test]
    fn attribution_version_reads_back_the_stamped_build() -> TestResult {
        let mut conn = fresh()?;
        assert_eq!(
            attribution_version_read_only(&conn)?,
            None,
            "a TS-era database carries no attribution version"
        );
        stamp_and_prepare(&mut conn, "build-abc-123", |_| Ok(0))?;
        assert_eq!(
            attribution_version_read_only(&conn)?,
            Some("build-abc-123".to_owned())
        );
        Ok(())
    }

    #[test]
    fn attribution_version_refuses_foreign_and_corrupt_epochs() -> TestResult {
        let conn = fresh()?;
        conn.execute_batch("PRAGMA user_version = 7")?;
        assert!(matches!(
            attribution_version_read_only(&conn),
            Err(EpochError::ForeignEpoch { found: 7 })
        ));

        let missing = fresh()?;
        missing.execute_batch("PRAGMA user_version = 2")?;
        assert!(matches!(
            attribution_version_read_only(&missing),
            Err(EpochError::CorruptAttribution { .. })
        ));

        let dup = fresh()?;
        dup.execute_batch(
            "PRAGMA user_version = 2;
             CREATE TABLE engine_epoch (epoch INTEGER NOT NULL, engine TEXT NOT NULL, version TEXT NOT NULL, time INTEGER NOT NULL);
             INSERT INTO engine_epoch VALUES (2, 'secode-rust', 'a', 0), (2, 'secode-rust', 'b', 0);",
        )?;
        assert!(matches!(
            attribution_version_read_only(&dup),
            Err(EpochError::CorruptAttribution { .. })
        ));
        Ok(())
    }

    #[test]
    fn attribution_version_read_leaves_the_database_file_untouched() -> TestResult {
        let ids = crate::ids::OsRandomIds::seed()?;
        let dir = std::env::temp_dir().join(format!("epoch-version-{:-}", ids.mint().as_str()));
        std::fs::create_dir(&dir)?;
        let db = dir.join("database.sqlite3");
        {
            let mut conn = Connection::open(&db)?;
            stamp_and_prepare(&mut conn, "build-file-1", |_| Ok(0))?;
        }
        let before = std::fs::read(&db)?;
        let conn = Connection::open(&db)?;
        assert_eq!(
            attribution_version_read_only(&conn)?,
            Some("build-file-1".to_owned())
        );
        drop(conn);
        assert_eq!(
            std::fs::read(&db)?,
            before,
            "the read-only attribution must not touch the file"
        );
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn attribution_version_at_reads_a_file_without_touching_it() -> TestResult {
        let ids = crate::ids::OsRandomIds::seed()?;
        let dir = std::env::temp_dir().join(format!("epoch-version-at-{:-}", ids.mint().as_str()));
        std::fs::create_dir(&dir)?;
        let db = dir.join("database.sqlite3");
        {
            let mut conn = Connection::open(&db)?;
            stamp_and_prepare(&mut conn, "build-file-2", |_| Ok(0))?;
        }
        let before = std::fs::read(&db)?;
        assert_eq!(
            attribution_version_at(&db)?,
            AttributionRead::Version("build-file-2".to_owned())
        );
        assert_eq!(
            std::fs::read(&db)?,
            before,
            "file read must not touch the bytes"
        );
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn attribution_version_at_names_the_foreign_and_missing_epochs() -> TestResult {
        let ids = crate::ids::OsRandomIds::seed()?;
        let dir = std::env::temp_dir().join(format!("epoch-version-at-{:-}", ids.mint().as_str()));
        std::fs::create_dir(&dir)?;
        let db = dir.join("database.sqlite3");
        {
            let conn = Connection::open(&db)?;
            conn.execute_batch("PRAGMA user_version = 7")?;
            conn.execute_batch("CREATE TABLE t (x INTEGER)")?;
        }
        assert!(matches!(
            attribution_version_at(&db),
            Err(AttributionError::ForeignEpoch { found: 7 })
        ));
        let db2 = dir.join("ts-era.sqlite3");
        Connection::open(&db2)?.execute_batch("CREATE TABLE t (x INTEGER)")?;
        assert_eq!(attribution_version_at(&db2)?, AttributionRead::TsEra);
        let missing = dir.join("absent.sqlite3");
        assert!(
            matches!(
                attribution_version_at(&missing),
                Err(AttributionError::Unreadable(rusqlite::Error::SqliteFailure(
                    failure,
                    _
                ))) if failure.code == rusqlite::ffi::ErrorCode::CannotOpen
            ),
            "a missing store file is unreadable, not corrupt or foreign"
        );
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}
