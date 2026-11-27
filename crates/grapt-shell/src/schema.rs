//! The shell tables a legacy store may already hold.
//!
//! Nothing populates a durable shell inventory: the live terminal owns its
//! running sessions in memory, and the product's `GET /api/shell` answers an
//! empty list. The tables are still created on a store that lacks them,
//! because a store written by an earlier build may carry shell rows, and this
//! build must open it and leave that data untouched rather than fail on a
//! missing table. This crate owns the shell payloads, so it owns this schema.

use grapt_store::EventStore;

/// Why the shell schema could not be installed or inspected.
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

const CREATE_TABLES: &str = "CREATE TABLE IF NOT EXISTS shell_operation (
            operation_id TEXT PRIMARY KEY,
            route_id TEXT NOT NULL CHECK (LENGTH(route_id) > 0),
            effective_graph_id TEXT NOT NULL CHECK (LENGTH(effective_graph_id) > 0),
            command TEXT NOT NULL CHECK (LENGTH(command) > 0),
            cwd TEXT NOT NULL CHECK (LENGTH(cwd) > 0),
            shell TEXT NOT NULL CHECK (LENGTH(shell) > 0),
            timeout_ms INTEGER CHECK (timeout_ms IS NULL OR timeout_ms >= 0),
            artifact_id TEXT NOT NULL CHECK (LENGTH(artifact_id) > 0),
            status TEXT NOT NULL CHECK (status IN
                ('admitted', 'running', 'exited', 'timed-out', 'cancelled', 'failed', 'ambiguous')),
            generation INTEGER,
            pid INTEGER,
            exit_code INTEGER,
            error TEXT,
            evidence TEXT,
            pipe TEXT NOT NULL CHECK (pipe IN ('open', 'drained', 'timed-out')),
            cleanup TEXT NOT NULL CHECK (cleanup IN ('pending', 'complete')),
            timeout_deadline INTEGER,
            time_created INTEGER NOT NULL,
            time_started INTEGER CHECK (time_started IS NULL OR time_started >= 0),
            time_updated INTEGER NOT NULL,
            -- Tagged state union: running carries a generation AND a pid;
            -- each terminal payload exists exactly in its own state.
            CHECK ((status = 'running') = (generation IS NOT NULL)),
            CHECK ((status = 'running') = (pid IS NOT NULL)),
            CHECK ((status = 'exited') = (exit_code IS NOT NULL)),
            CHECK ((status = 'failed') = (error IS NOT NULL)),
            CHECK ((status = 'ambiguous') = (evidence IS NOT NULL)),
            -- Cleanup completes only after the pipe has settled. Pipe and
            -- status are otherwise INDEPENDENT: an exited operation may have
            -- an open pipe and pending cleanup.
            CHECK (cleanup = 'pending' OR pipe IN ('drained', 'timed-out'))
        );
        CREATE TABLE IF NOT EXISTS shell_artifact (
            artifact_id TEXT PRIMARY KEY CHECK (LENGTH(artifact_id) > 0),
            cap INTEGER NOT NULL CHECK (cap > 0),
            size INTEGER NOT NULL CHECK (size >= 0 AND size <= cap),
            truncated INTEGER NOT NULL CHECK (truncated IN (0, 1)),
            bytes BLOB NOT NULL CHECK (LENGTH(bytes) = size)
        );
        CREATE TABLE IF NOT EXISTS product_shell_binding (
            operation_id TEXT PRIMARY KEY NOT NULL,
            shell_id TEXT UNIQUE NOT NULL,
            file TEXT NOT NULL,
            metadata TEXT NOT NULL
        );";

/// The migration an older store still needs: `shell_operation` gained
/// `time_started` after its first release.
const ADD_TIME_STARTED: &str = "ALTER TABLE shell_operation ADD COLUMN time_started INTEGER CHECK (time_started IS NULL OR time_started >= 0)";

pub fn initialize(store: &mut EventStore) -> Result<(), SchemaError> {
    // Read-only first: a current schema needs no transaction at all. Any
    // creation or upgrade then happens in ONE governed transaction, so a
    // fresh database pays one durable commit instead of one per statement.
    if current(store.connection())? {
        return Ok(());
    }
    let tx = store.governed_transaction()?;
    initialize_within(&tx)?;
    tx.commit()?.discard();
    Ok(())
}

/// [`initialize`] inside a transaction the caller holds.
pub fn initialize_within(tx: &grapt_store::GovernedTransaction<'_>) -> Result<(), SchemaError> {
    if current(tx)? {
        return Ok(());
    }
    create(tx)?;
    Ok(())
}

/// Whether the store carries every object [`create`] installs. Read-only.
fn current(conn: &rusqlite::Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT (SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'
                     AND name IN ('shell_operation', 'shell_artifact', 'product_shell_binding')) = 3
            AND EXISTS(SELECT 1 FROM pragma_table_info('shell_operation')
                       WHERE name = 'time_started')",
        [],
        |row| row.get(0),
    )
}

fn create(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute_batch(CREATE_TABLES)?;
    let has_started: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('shell_operation') WHERE name = 'time_started')",
        [],
        |row| row.get(0),
    )?;
    if !has_started {
        conn.execute_batch(ADD_TIME_STARTED)?;
    }
    Ok(())
}
