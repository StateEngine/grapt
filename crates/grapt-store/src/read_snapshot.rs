//! Scoped read transactions on the existing read-only event-log owner.
use crate::EventLog;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::{Connection, Statement};
use std::cell::Cell;
use std::time::Duration;

/// Only prepared reads escape to the history reader, never the connection or
/// transaction handle. The owner ends the snapshot when its callback returns.
#[derive(Debug)]
pub struct ReadSnapshot<'a> {
    connection: &'a mut Connection,
    statements: Cell<u32>,
    active: bool,
}

impl ReadSnapshot<'_> {
    pub fn prepare(&self, sql: &str) -> rusqlite::Result<Statement<'_>> {
        if sql.len() > 4096 || self.statements.get() == 128 {
            return Err(rusqlite::Error::InvalidParameterName(
                "read snapshot exceeds SQL or statement budget".into(),
            ));
        }
        self.statements.set(self.statements.get() + 1);
        self.connection.prepare(sql)
    }

    fn release_hooks(&mut self) -> rusqlite::Result<()> {
        let progress = self.connection.progress_handler(0, None::<fn() -> bool>);
        let authorization = self
            .connection
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
        progress.and(authorization)
    }
}

impl Drop for ReadSnapshot<'_> {
    fn drop(&mut self) {
        // Normal completion reports cleanup failures below. Unwinding still
        // removes the denial before rolling back the owned read transaction.
        let _ = self.release_hooks();
        if self.active {
            let _ = self.connection.execute_batch("ROLLBACK");
        }
    }
}

impl EventLog {
    /// Zero busy wait; at most 128 statements, 4096 SQL bytes per statement and
    /// ten million SQLite VM operations in one synchronous callback. The caller
    /// separately bounds returned rows/data; no cursor or transaction escapes.
    pub fn with_read_snapshot<T>(
        &self,
        read: impl FnOnce(&ReadSnapshot<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut connection = self
            .conn
            .try_borrow_mut()
            .map_err(|_| "read snapshot is already active".to_owned())?;
        if !connection.is_autocommit() {
            return Err("read snapshot is already active".into());
        }
        connection
            .busy_timeout(Duration::ZERO)
            .map_err(|e| e.to_string())?;
        connection
            .execute_batch("BEGIN DEFERRED")
            .map_err(|e| format!("read snapshot begin failed: {e}"))?;
        let mut snapshot = ReadSnapshot::new(&mut connection, true)?;
        let result = read(&snapshot);
        let cleanup = snapshot.release_hooks();
        let rollback = snapshot.connection.execute_batch("ROLLBACK");
        snapshot.active = rollback.is_err();
        drop(snapshot);
        cleanup.map_err(|e| format!("read snapshot hook cleanup failed: {e}"))?;
        rollback.map_err(|e| format!("read snapshot release failed: {e}"))?;
        result
    }
}

impl<'a> ReadSnapshot<'a> {
    fn new(connection: &'a mut Connection, active: bool) -> Result<Self, String> {
        let snapshot = Self {
            connection,
            statements: Cell::new(0),
            active,
        };
        snapshot
            .connection
            .authorizer(Some(read_authorization))
            .map_err(|error| error.to_string())?;
        let mut callbacks_left = 10_000_u32;
        snapshot
            .connection
            .progress_handler(
                1000,
                Some(move || {
                    callbacks_left = callbacks_left.saturating_sub(1);
                    callbacks_left == 0
                }),
            )
            .map_err(|error| error.to_string())?;
        Ok(snapshot)
    }

    pub(crate) fn pooled(connection: &'a mut Connection) -> Result<Self, String> {
        if connection.is_autocommit()
            || !connection
                .is_readonly(rusqlite::MAIN_DB)
                .map_err(|error| error.to_string())?
        {
            return Err("accounting requires an active read-only snapshot".into());
        }
        Self::new(connection, false)
    }
}

fn read_authorization(context: AuthContext<'_>) -> Authorization {
    match context.action {
        AuthAction::Read { .. } | AuthAction::Select | AuthAction::Recursive => {
            Authorization::Allow
        }
        AuthAction::Function {
            function_name: "length" | "count",
        } => Authorization::Allow,
        _ => Authorization::Deny,
    }
}
