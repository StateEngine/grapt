//! The run store's schema ledger. A build supplies its schema as an ordered
//! list of migrations, versions 1, 2, 3, ...; the database records each one it
//! applied in `rust_migration` with a checksum of its SQL, and mirrors the
//! newest in `user_version`.
//!
//! Opening refuses, on a read-only connection and before any change, a file
//! that is not a run store, was migrated by a newer build, or records a
//! migration whose SQL differs from this build's. Pending migrations then run
//! under the write lock, behind a verified backup taken while that lock is
//! held, in one transaction with their ledger rows, so a failure leaves the
//! database as it was. SQLite refuses transaction control, attachment,
//! pragmas and access to the store's own tables from migration SQL.
use crate::error::{LedgerError, OpenError};
use crate::sqlite::{self, INTERNAL, LEDGER, RECEIPTS_DDL};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

mod backup;

const APPLICATION_ID: i64 = 0x4752_4150;

/// One step of a run database's schema, compiled into the build that owns it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MigrationSpec {
    version: u32,
    name: &'static str,
    sql: &'static str,
}

impl MigrationSpec {
    /// `sql` is trusted DDL and data movement. It runs inside the store's
    /// transaction, so it must not contain `BEGIN`, `COMMIT` or `ROLLBACK`.
    pub const fn new(version: u32, name: &'static str, sql: &'static str) -> Self {
        Self { version, name, sql }
    }

    fn checksum(&self) -> [u8; 32] {
        Sha256::digest(self.sql.as_bytes()).into()
    }
}

/// What opening did to the schema.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Migrated {
    /// The version the database had; 0 for a new file.
    pub from: u32,
    /// The version it has now.
    pub to: u32,
    /// The verified copy taken before an existing database was changed.
    pub backup: Option<PathBuf>,
}

/// The read-only verdict on a database before anything is written to it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    from: u32,
    fresh: bool,
}

impl Plan {
    /// A database file that does not exist yet.
    pub(crate) const NEW: Self = Self {
        from: 0,
        fresh: true,
    };
}

/// Check `ledger` and the database against each other without changing either.
pub(crate) fn plan(connection: &Connection, ledger: &[MigrationSpec]) -> Result<Plan, OpenError> {
    validate(ledger)?;
    let max_supported = ledger.len() as u32;
    let app: i64 = connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let found: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let empty: bool = connection.query_row(
        "SELECT NOT EXISTS (SELECT 1 FROM sqlite_schema)",
        [],
        |row| row.get(0),
    )?;
    if app == 0 && found == 0 && empty {
        return Ok(Plan::NEW);
    }
    if app != APPLICATION_ID {
        return Err(OpenError::Schema(found));
    }
    if found > i64::from(max_supported) {
        return Err(OpenError::FutureSchemaVersion {
            found,
            max_supported,
        });
    }
    let applied = applied(connection, max_supported)?;
    let newest = applied.last().map_or(0, |row| row.0);
    if newest > i64::from(max_supported) {
        return Err(OpenError::FutureSchemaVersion {
            found: newest,
            max_supported,
        });
    }
    for (index, (version, name, checksum)) in applied.iter().enumerate() {
        if *version != index as i64 + 1 {
            return Err(LedgerError::Inconsistent { version: *version }.into());
        }
        let spec = &ledger[index];
        if name != spec.name || checksum[..] != spec.checksum() {
            return Err(LedgerError::Changed {
                version: spec.version,
            }
            .into());
        }
    }
    if newest != found || found < 1 {
        return Err(LedgerError::Inconsistent { version: found }.into());
    }
    Ok(Plan {
        from: found as u32,
        fresh: false,
    })
}

/// Versions must run 1, 2, 3, ... so the newest is also the count.
fn validate(ledger: &[MigrationSpec]) -> Result<(), LedgerError> {
    if ledger.is_empty() {
        return Err(LedgerError::Spec(
            "a run store needs at least one migration",
        ));
    }
    // `user_version` is a signed 32-bit header field.
    if ledger.len() > i32::MAX as usize {
        return Err(LedgerError::Spec("too many migrations"));
    }
    for (index, spec) in ledger.iter().enumerate() {
        if spec.version as usize != index + 1 {
            return Err(LedgerError::Spec(
                "versions must run 1, 2, 3, ... without gaps",
            ));
        }
        if spec.name.is_empty() {
            return Err(LedgerError::Spec("every migration needs a name"));
        }
    }
    Ok(())
}

type Applied = (i64, String, Vec<u8>);

/// At most one row past `max_supported`: any such row already refuses.
fn applied(connection: &Connection, max_supported: u32) -> Result<Vec<Applied>, OpenError> {
    let present = connection
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [LEDGER],
            |_| Ok(()),
        )
        .optional()?;
    if present.is_none() {
        return Err(LedgerError::Missing.into());
    }
    let rows = connection
        .prepare(&format!(
            "SELECT version, name, checksum FROM {LEDGER} ORDER BY version LIMIT ?1"
        ))?
        .query_map([i64::from(max_supported) + 1], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<Vec<Applied>>>()?;
    Ok(rows)
}

/// Apply what `plan` found pending. `source` is the read-only connection the
/// plan was made on, kept to back up an existing store; `path` is the
/// database's real location, beside which the backup is written.
pub(crate) fn apply(
    connection: &mut Connection,
    source: Option<Connection>,
    path: &Path,
    ledger: &[MigrationSpec],
    planned: Plan,
) -> Result<Migrated, OpenError> {
    let to = ledger.len() as u32;
    if planned.from == to {
        return Ok(Migrated {
            from: to,
            to,
            backup: None,
        });
    }
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = migrate(connection, source, path, ledger, &planned);
    let result = result.and_then(|backup| {
        connection.execute_batch("COMMIT")?;
        Ok(backup)
    });
    if result.is_err() && !connection.is_autocommit() {
        let _ = connection.execute_batch("ROLLBACK");
    }
    result.map(|backup| Migrated {
        from: planned.from,
        to,
        backup,
    })
}

/// Runs holding the write lock, so no writer can commit between the backup
/// and the migrations it protects.
fn migrate(
    connection: &mut Connection,
    source: Option<Connection>,
    path: &Path,
    ledger: &[MigrationSpec],
    planned: &Plan,
) -> Result<Option<PathBuf>, OpenError> {
    if plan(connection, ledger)? != *planned {
        return Err(LedgerError::Moved.into());
    }
    let backup = match source {
        Some(source) if !planned.fresh => Some(backup::take(&source, path, planned.from)?),
        _ => None,
    };
    if planned.fresh {
        connection.execute_batch(RECEIPTS_DDL)?;
        connection.execute_batch(&format!(
            "CREATE TABLE {LEDGER} (
                version INTEGER PRIMARY KEY CHECK(version > 0),
                name TEXT NOT NULL,
                checksum BLOB NOT NULL CHECK(length(checksum) = 32)
            );"
        ))?;
    }
    for spec in &ledger[planned.from as usize..] {
        run(connection, spec)?;
        connection.execute(
            &format!("INSERT INTO {LEDGER} (version, name, checksum) VALUES (?1, ?2, ?3)"),
            params![spec.version, spec.name, spec.checksum()],
        )?;
    }
    connection.pragma_update(None, "application_id", APPLICATION_ID)?;
    connection.pragma_update(None, "user_version", ledger.len() as u32)?;
    sqlite::verify_pager(connection)?;
    Ok(backup)
}

/// Run one migration's SQL with SQLite refusing, before it executes, anything
/// that could escape the store's transaction or reach the store's own tables.
fn run(connection: &mut Connection, spec: &MigrationSpec) -> Result<(), OpenError> {
    connection.authorizer(Some(migration_sql))?;
    let result = connection.execute_batch(spec.sql);
    connection.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
    result.map_err(|error| match OpenError::from(error) {
        OpenError::Sqlite(source) => OpenError::Migration {
            version: spec.version,
            source,
        },
        pressure => pressure,
    })
}

fn migration_sql(context: AuthContext<'_>) -> Authorization {
    let internal = |table: &str| INTERNAL.iter().any(|name| table.eq_ignore_ascii_case(name));
    match context.action {
        AuthAction::Transaction { .. }
        | AuthAction::Savepoint { .. }
        | AuthAction::Attach { .. }
        | AuthAction::Detach { .. }
        | AuthAction::Pragma { .. } => Authorization::Deny,
        AuthAction::Function {
            function_name: "load_extension",
        } => Authorization::Deny,
        AuthAction::Read { table_name, .. }
        | AuthAction::Insert { table_name }
        | AuthAction::Update { table_name, .. }
        | AuthAction::Delete { table_name }
        | AuthAction::AlterTable { table_name, .. }
        | AuthAction::DropTable { table_name }
        | AuthAction::CreateIndex { table_name, .. }
        | AuthAction::DropIndex { table_name, .. }
        | AuthAction::CreateTrigger { table_name, .. }
        | AuthAction::DropTrigger { table_name, .. }
        | AuthAction::Analyze { table_name }
            if internal(table_name) =>
        {
            Authorization::Deny
        }
        _ => Authorization::Allow,
    }
}
