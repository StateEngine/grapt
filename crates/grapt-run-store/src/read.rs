use crate::command::Receipt;
use crate::error::ReadError;
use crate::key::RequestKey;
use crate::{sqlite, transaction};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::{Connection, Statement};
use std::cell::Cell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::time::{timeout_at, Instant};

const READERS: u32 = 16;
const ACTIVE_LIMIT: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct Pool {
    path: PathBuf,
    permits: Arc<Semaphore>,
    idle: Mutex<Vec<Connection>>,
    closed: AtomicBool,
}

#[derive(Clone, Debug)]
pub struct ReadPool(Arc<Pool>);

impl ReadPool {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self(Arc::new(Pool {
            path,
            permits: Arc::new(Semaphore::new(READERS as usize)),
            idle: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
        }))
    }

    /// Checkout has its own deadline. An acquired snapshot gets up to five
    /// seconds, with SQL interrupted on cancellation or expiry. Callbacks are
    /// synchronous, bounded projections and must return owned results.
    pub async fn snapshot<T: Send + 'static>(
        &self,
        checkout_deadline: Instant,
        project: impl FnOnce(&ReadView<'_>) -> Result<T, ReadError> + Send + 'static,
    ) -> Result<T, ReadError> {
        if self.0.closed.load(Ordering::Acquire) {
            return Err(ReadError::Closed);
        }
        if checkout_deadline <= Instant::now() {
            return Err(ReadError::Busy);
        }
        let permit = timeout_at(
            checkout_deadline,
            Arc::clone(&self.0.permits).acquire_owned(),
        )
        .await
        .map_err(|_| ReadError::Busy)?
        .map_err(|_| ReadError::Closed)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_on_drop = Cancel(Arc::clone(&cancelled));
        let pool = Arc::clone(&self.0);
        let (started, ready) = tokio::sync::oneshot::channel();
        let job = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            if pool.closed.load(Ordering::Acquire) {
                return Err(ReadError::Closed);
            }
            let start = Instant::now();
            if start >= checkout_deadline || cancelled.load(Ordering::Acquire) {
                return Err(ReadError::Busy);
            }
            let end = start + ACTIVE_LIMIT;
            if started.send(end).is_err() {
                return Err(ReadError::Busy);
            }
            pool.project(project, cancelled, end)
        });
        let end = match timeout_at(checkout_deadline, ready).await {
            Ok(Ok(end)) => end,
            Ok(Err(_)) => return job.await.map_err(|_| ReadError::Worker)?,
            Err(_) => return Err(ReadError::Busy),
        };
        let result = timeout_at(end, job)
            .await
            .map_err(|_| ReadError::Busy)?
            .map_err(|_| ReadError::Worker)?;
        drop(cancel_on_drop);
        result
    }

    /// An absent receipt can still be in flight.
    pub async fn resolve(
        &self,
        key: RequestKey,
        deadline: Instant,
    ) -> Result<Option<Receipt>, ReadError> {
        self.snapshot(deadline, move |read| read.receipt(&key))
            .await
    }

    pub(crate) fn stop(&self) {
        self.0.closed.store(true, Ordering::Release);
    }

    pub(crate) fn close_idle(&self) -> Result<(), ReadError> {
        self.0.close_idle()
    }

    pub(crate) async fn close(&self) -> Result<(), ReadError> {
        self.stop();
        let _permits = Arc::clone(&self.0.permits)
            .acquire_many_owned(READERS)
            .await
            .map_err(|_| ReadError::Closed)?;
        self.0.permits.close();
        let pool = Arc::clone(&self.0);
        tokio::task::spawn_blocking(move || pool.close_idle())
            .await
            .map_err(|_| ReadError::Worker)?
    }
}

struct Cancel(Arc<AtomicBool>);

impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl Pool {
    fn close_idle(&self) -> Result<(), ReadError> {
        let connections = std::mem::take(&mut *self.idle.lock().map_err(|_| ReadError::Worker)?);
        drop(connections);
        Ok(())
    }

    fn project<T>(
        &self,
        project: impl FnOnce(&ReadView<'_>) -> Result<T, ReadError>,
        cancelled: Arc<AtomicBool>,
        end: Instant,
    ) -> Result<T, ReadError> {
        let cached = self.idle.lock().map_err(|_| ReadError::Worker)?.pop();
        let mut connection = match cached {
            Some(connection) => connection,
            None => sqlite::reader(&self.path).map_err(sqlite_error)?,
        };
        connection
            .execute_batch("BEGIN DEFERRED")
            .map_err(sqlite_error)?;
        connection
            .query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))
            .map_err(sqlite_error)?;
        connection
            .authorizer(Some(read_only))
            .map_err(sqlite_error)?;
        let active = Arc::clone(&cancelled);
        connection
            .progress_handler(
                1000,
                Some(move || active.load(Ordering::Acquire) || Instant::now() >= end),
            )
            .map_err(sqlite_error)?;
        let result = project(&ReadView {
            connection: &connection,
            statements: Cell::new(0),
        });
        connection
            .progress_handler(0, None::<fn() -> bool>)
            .map_err(sqlite_error)?;
        connection
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
            .map_err(sqlite_error)?;
        connection.execute_batch("ROLLBACK").map_err(sqlite_error)?;
        let value = result?;
        if cancelled.load(Ordering::Acquire) || Instant::now() >= end {
            return Err(ReadError::Busy);
        }
        let mut cache = self.idle.lock().map_err(|_| ReadError::Worker)?;
        if !self.closed.load(Ordering::Acquire) {
            cache.push(connection);
        }
        Ok(value)
    }
}

fn read_only(context: AuthContext<'_>) -> Authorization {
    match context.action {
        // The schema ledger belongs to the store; receipts stay readable for
        // the pool's own receipt lookups.
        AuthAction::Read { table_name, .. } if table_name.eq_ignore_ascii_case(sqlite::LEDGER) => {
            Authorization::Deny
        }
        AuthAction::Select | AuthAction::Read { .. } | AuthAction::Recursive => {
            Authorization::Allow
        }
        AuthAction::Function { function_name } if function_name != "load_extension" => {
            Authorization::Allow
        }
        _ => Authorization::Deny,
    }
}

fn sqlite_error(error: rusqlite::Error) -> ReadError {
    match error.sqlite_error_code() {
        Some(
            rusqlite::ErrorCode::DatabaseBusy
            | rusqlite::ErrorCode::DatabaseLocked
            | rusqlite::ErrorCode::OperationInterrupted,
        ) => ReadError::Busy,
        _ => error.into(),
    }
}

/// One read-only snapshot. Prepared statements borrow it and cannot escape
/// the blocking job. Each callback can prepare at most 128 statements.
#[derive(Debug)]
pub struct ReadView<'a> {
    connection: &'a Connection,
    statements: Cell<u8>,
}

impl ReadView<'_> {
    pub fn prepare(&self, sql: &str) -> Result<Statement<'_>, ReadError> {
        if self.statements.get() >= 128 || sql.len() > 4096 {
            return Err(ReadError::Sqlite(rusqlite::Error::InvalidQuery));
        }
        self.statements.set(self.statements.get() + 1);
        self.connection.prepare(sql).map_err(sqlite_error)
    }

    fn receipt(&self, key: &RequestKey) -> Result<Option<Receipt>, ReadError> {
        let Some((_, bytes)) = transaction::lookup(self.connection, key).map_err(sqlite_error)?
        else {
            return Ok(None);
        };
        Receipt::decode(&bytes).map(Some).ok_or(ReadError::Receipt)
    }
}

#[cfg(test)]
mod tests {
    use crate::RunStore;
    use std::sync::mpsc;
    use std::time::Duration;

    #[tokio::test]
    async fn canceled_shutdown_drops_while_the_reader_cache_is_locked(
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let folder = tempfile::TempDir::new()?;
        const EMPTY: &[crate::MigrationSpec] = &[crate::MigrationSpec::new(1, "empty", "")];
        let store = RunStore::open(folder.path().join("run.db"), EMPTY).await?;
        let readers = store.readers();
        let pool = readers.0.clone();
        let (held, locked) = tokio::sync::oneshot::channel();
        let (release, proceed) = mpsc::sync_channel(1);
        let blocker = std::thread::spawn(move || {
            let _held = pool.idle.lock().map_err(|_| "reader cache poisoned")?;
            held.send(()).map_err(|_| "read synchronization stopped")?;
            proceed
                .recv_timeout(Duration::from_secs(1))
                .map_err(|_| "shutdown waited for the cache")?;
            Ok::<(), &'static str>(())
        });
        locked.await?;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), store.shutdown())
                .await
                .is_err()
        );
        release.send(())?;
        blocker.join().map_err(|_| "cache fixture panicked")??;
        Ok(())
    }
}
