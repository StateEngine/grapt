use crate::command::{MAX_COMMAND_BYTES, MAX_PARAMS, MAX_SQL_BYTES};
use crate::error::{OpenError, WriteError};
use crate::wal_budget::{WalBudget, CACHE_KIB};
use rusqlite::config::DbConfig;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::limits::Limit;
use rusqlite::{Connection, OpenFlags};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;
use std::time::Duration;

mod plan;

pub(crate) use plan::check_write_plan;

pub(crate) const RECEIPTS: &str = "run_store_receipt";
pub(crate) const LEDGER: &str = "rust_migration";
/// Storage-owned tables that domain SQL can neither read nor write.
pub(crate) const INTERNAL: [&str; 2] = [RECEIPTS, LEDGER];
pub(crate) const RECEIPTS_DDL: &str = "CREATE TABLE run_store_receipt (
    request_key TEXT PRIMARY KEY,
    digest BLOB NOT NULL CHECK(length(digest) = 32),
    receipt BLOB NOT NULL
) WITHOUT ROWID;";
pub(crate) const WRITE_BUSY: Duration = Duration::from_millis(50);
// The transfer optimizer can copy index entries in a separate VM loop.
// Sample that loop as well as the ordinary row callbacks.
const PROGRESS_STEPS: i32 = 8;
const MAX_PROGRESS_CALLS: u16 = 32_000;

pub(crate) fn lock_writer(path: &Path) -> Result<(PathBuf, File), OpenError> {
    let path = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let name = path.file_name().ok_or(error)?;
            let parent = path.parent().filter(|path| !path.as_os_str().is_empty());
            fs::canonicalize(parent.unwrap_or(Path::new(".")))?.join(name)
        }
        Err(error) => return Err(error.into()),
    };
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(sidecar(&path, ".writer-lock"))?;
    match fs2::FileExt::try_lock_exclusive(&lease) {
        Ok(()) => Ok((path, lease)),
        Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            Err(OpenError::WriterExists)
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    name.into()
}

/// The writer's connection, before its schema is checked or changed.
pub(crate) fn writer(path: &Path) -> Result<Connection, OpenError> {
    let connection = Connection::open(path)?;
    // A file refused before its migrations commit must close as it was found,
    // so closing never checkpoints or deletes its WAL until `enable_wal`.
    checkpoint_on_close(&connection, false)?;
    connection.busy_timeout(WRITE_BUSY)?;
    connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, MAX_COMMAND_BYTES as i32)?;
    connection.execute_batch(
        "PRAGMA temp_store=FILE; PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF;
         PRAGMA synchronous=FULL;",
    )?;
    #[cfg(test)]
    page_limit::apply(&connection, path)?;
    Ok(connection)
}

/// Switch the run store to WAL and let its last close checkpoint again. Runs
/// only after migrations commit, so a file that is refused, or whose backup
/// fails, keeps its journal mode and its WAL.
pub(crate) fn enable_wal(connection: &Connection) -> Result<(), OpenError> {
    verify_pager(connection)?;
    checkpoint_on_close(connection, true)?;
    let mode: String = connection.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    if mode != "wal" {
        return Err(OpenError::JournalMode(mode));
    }
    Ok(())
}

/// The writer's only SQLite configuration change: whether its close may
/// checkpoint and remove the WAL.
fn checkpoint_on_close(connection: &Connection, enabled: bool) -> Result<(), OpenError> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the run store's own writer; it changes no schema protection"
    )]
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, !enabled)?;
    Ok(())
}

/// The limits live writes run under, set once migrations are done.
pub(crate) fn restrict(connection: &Connection) -> Result<(), OpenError> {
    connection.execute_batch(
        "PRAGMA temp_store=MEMORY; PRAGMA wal_autocheckpoint=0;
         PRAGMA cache_spill=ON; PRAGMA mmap_size=0; PRAGMA threads=0;",
    )?;
    connection.pragma_update(None, "cache_size", -CACHE_KIB)?;
    connection.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)?;
    connection.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, MAX_SQL_BYTES as i32)?;
    connection.set_limit(Limit::SQLITE_LIMIT_FUNCTION_ARG, 8)?;
    connection.set_limit(Limit::SQLITE_LIMIT_EXPR_DEPTH, 16)?;
    connection.set_limit(Limit::SQLITE_LIMIT_COLUMN, 32)?;
    connection.set_limit(Limit::SQLITE_LIMIT_VARIABLE_NUMBER, MAX_PARAMS as i32)?;
    connection.set_limit(Limit::SQLITE_LIMIT_VDBE_OP, 1024)?;
    Ok(())
}

pub(crate) fn verify_pager(connection: &Connection) -> Result<(), OpenError> {
    let auto_vacuum: i64 = connection.pragma_query_value(None, "auto_vacuum", |row| row.get(0))?;
    if auto_vacuum != 0 {
        return Err(OpenError::AutoVacuum);
    }
    Ok(())
}

pub(crate) fn reader(path: &Path) -> rusqlite::Result<Connection> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.execute_batch(
        "PRAGMA query_only=ON; PRAGMA temp_store=FILE; PRAGMA trusted_schema=OFF;
         PRAGMA cache_size=-2048; PRAGMA mmap_size=0; PRAGMA threads=0;",
    )?;
    connection.busy_timeout(Duration::ZERO)?;
    Ok(connection)
}

/// Only storage-owned statements execute with the authorizer in control mode.
/// The mode is restored before SAVEPOINT release or receipt insertion.
pub(crate) struct Guard {
    budget: Arc<WalBudget>,
    active: AtomicBool,
    vm_left: AtomicU16,
}

pub(crate) struct DomainSql<'a>(&'a Guard);

impl Guard {
    pub(crate) fn enter(&self) -> DomainSql<'_> {
        self.budget.start_request();
        self.vm_left.store(MAX_PROGRESS_CALLS, Ordering::Relaxed);
        self.active.store(true, Ordering::Relaxed);
        DomainSql(self)
    }
    pub(crate) fn check_wal(&self) -> Result<(), WriteError> {
        self.budget.check()
    }

    pub(crate) fn take_refusal(&self) -> Option<WriteError> {
        self.budget.take_refusal()
    }
}

impl Drop for DomainSql<'_> {
    fn drop(&mut self) {
        self.0.active.store(false, Ordering::Relaxed);
    }
}

pub(crate) fn install_guard(
    connection: &mut Connection,
    budget: Arc<WalBudget>,
) -> rusqlite::Result<Arc<Guard>> {
    let guard = Arc::new(Guard {
        budget,
        active: AtomicBool::new(false),
        vm_left: AtomicU16::new(0),
    });
    let auth = Arc::clone(&guard);
    connection.authorizer(Some(move |context: AuthContext<'_>| {
        if !auth.active.load(Ordering::Relaxed) {
            return Authorization::Allow;
        }
        match context.action {
            AuthAction::Read { table_name, .. }
                if !INTERNAL
                    .iter()
                    .any(|internal| table_name.eq_ignore_ascii_case(internal))
                    && !table_name.starts_with("sqlite_")
                    && context.database_name == Some("main") =>
            {
                Authorization::Allow
            }
            AuthAction::Insert { table_name } | AuthAction::Update { table_name, .. }
                if context.database_name == Some("main") && auth.budget.can_write(table_name) =>
            {
                Authorization::Allow
            }
            // Disable the truncate fast path so DELETE runs the row callback.
            AuthAction::Delete { table_name }
                if context.database_name == Some("main") && auth.budget.can_write(table_name) =>
            {
                Authorization::Ignore
            }
            AuthAction::Select | AuthAction::Recursive => Authorization::Allow,
            AuthAction::Function { function_name }
                if !["load_extension", "sqlite_rename_table"].contains(&function_name) =>
            {
                Authorization::Allow
            }
            _ => Authorization::Deny,
        }
    }))?;
    let progress = Arc::clone(&guard);
    connection.progress_handler(
        PROGRESS_STEPS,
        Some(move || {
            if !progress.active.load(Ordering::Relaxed) {
                return false;
            }
            progress.budget.interrupted()
                || progress
                    .vm_left
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                        left.checked_sub(1)
                    })
                    .map_or(true, |left| left <= 1)
        }),
    )?;
    let rows = Arc::clone(&guard);
    let interrupt = connection.get_interrupt_handle();
    connection.update_hook(Some(move |_, schema: &str, table: &str, _| {
        if schema == "main" && rows.active.load(Ordering::Relaxed) && rows.budget.changed(table) {
            interrupt.interrupt();
        }
    }))?;
    Ok(guard)
}

/// Test-only: cap one database's pages on its writer connection, so SQLite
/// itself reports a full disk on the real open path. Not in other builds.
#[cfg(test)]
pub(crate) mod page_limit {
    use rusqlite::Connection;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    static LIMIT: Mutex<Option<(PathBuf, u32)>> = Mutex::new(None);

    /// Holds the cap on the canonical `path` until dropped.
    pub(crate) struct PageLimit;

    impl PageLimit {
        pub(crate) fn set(path: &Path, pages: u32) -> std::io::Result<Self> {
            let path = std::fs::canonicalize(path)?;
            *LIMIT.lock().map_err(|_| std::io::ErrorKind::Other)? = Some((path, pages));
            Ok(Self)
        }
    }

    impl Drop for PageLimit {
        fn drop(&mut self) {
            if let Ok(mut limit) = LIMIT.lock() {
                *limit = None;
            }
        }
    }

    pub(super) fn apply(connection: &Connection, path: &Path) -> rusqlite::Result<()> {
        let limit = LIMIT.lock().ok().and_then(|limit| limit.clone());
        match limit {
            Some((limited, pages)) if limited == path => {
                connection.pragma_update(None, "max_page_count", pages)
            }
            _ => Ok(()),
        }
    }
}
