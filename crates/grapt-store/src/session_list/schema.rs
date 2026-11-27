use rusqlite::{Connection, Transaction};

use super::budget::DeferredVmBudget;
use super::EMPTY_SHA256;

/// The columns a table named `session` must carry to BE a legacy session
/// source. One list serves both the migration preflight and the open guard, so
/// "is this a legacy source" cannot mean two different things in the two places
/// that ask it.
pub(super) const LEGACY_SESSION_COLUMNS: [&str; 7] = [
    "id",
    "project_id",
    "workspace_id",
    "parent_id",
    "directory",
    "title",
    "time_updated",
];

/// True only for a table named `session` that carries the legacy shape AND
/// holds at least one row — the exact state whose rows an `empty-rust-store`
/// projection would hide.
fn legacy_source_is_populated(conn: &Connection) -> rusqlite::Result<bool> {
    if !table_exists(conn, "session")? {
        return Ok(false);
    }
    let columns = columns_of(conn, "session")?;
    if !LEGACY_SESSION_COLUMNS
        .iter()
        .all(|required| columns.iter().any(|present| present == required))
    {
        return Ok(false);
    }
    // EXISTS, not COUNT(*): the question is whether ANY row is at risk, and a
    // full count over a large legacy table would be unbounded work at open.
    conn.query_row("SELECT EXISTS(SELECT 1 FROM session)", [], |row| {
        row.get::<_, i64>(0)
    })
    .map(|value| value != 0)
}

pub(super) fn table_exists(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get::<_, i64>(0),
    )
    .map(|value| value != 0)
}

pub(super) fn columns_of(conn: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect();
    columns
}

fn verify_index(
    conn: &Connection,
    name: &str,
    expected: &[&str],
) -> Result<(), SessionListSchemaError> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
        [name],
        |row| row.get(0),
    )?;
    if exists != 1 {
        return Err(SessionListSchemaError::Corrupt(format!(
            "required index {name} is missing"
        )));
    }
    let mut statement = conn.prepare(&format!("PRAGMA index_xinfo(\"{name}\")"))?;
    let actual = statement
        .query_map([], |row| {
            let key: i64 = row.get(5)?;
            if key == 0 {
                Ok(None)
            } else {
                Ok(Some((row.get::<_, String>(2)?, row.get::<_, String>(4)?)))
            }
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let actual_columns = actual
        .iter()
        .map(|(column, _)| column.as_str())
        .collect::<Vec<_>>();
    if actual_columns != expected {
        return Err(SessionListSchemaError::Corrupt(format!(
            "index {name} columns are {actual_columns:?}, expected {expected:?}"
        )));
    }
    if actual.iter().any(|(_, collation)| collation != "BINARY") {
        return Err(SessionListSchemaError::Corrupt(format!(
            "index {name} collations are {actual:?}, expected BINARY"
        )));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum SessionListSchemaError {
    #[error("session-list projection is unavailable")]
    Unavailable,
    #[error("session-list projection is corrupt: {0}")]
    Corrupt(String),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

pub(crate) fn ensure_schema(
    tx: &Transaction<'_>,
    budget: &DeferredVmBudget,
) -> rusqlite::Result<()> {
    if table_exists(tx, "session_list_projection")? {
        // The open/restart check runs under its OWN declared bound. It proves
        // the projection's shape only: a row that breaks the projection law
        // is skipped and reported by the page that reads it, so one bad row
        // can no longer keep the whole store from opening.
        let guard = budget.enter()?;
        let verdict = verify_schema_shape(tx);
        let exhausted = budget.exhausted();
        drop(guard);
        if exhausted {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                Some("session-list open verification exceeded its SQLite VM-step bound".to_owned()),
            ));
        }
        return verdict.map_err(|_| rusqlite::Error::InvalidQuery);
    }
    // An ABSENT projection beside a PRESENT legacy `session` table is not an
    // empty page. Creating the tables here would stamp `empty-rust-store`
    // provenance over populated legacy state, and the open would SUCCEED — so
    // the shipped sessions would be neither imported nor reported, just
    // invisible behind a projection claiming the store was always empty. The
    // one admitted route from legacy state to this projection is
    // `ProductionMigrations`, which reads the source, records its real domain
    // and settles the baseline atomically. This open refuses instead, and says
    // why, so the database is migrated rather than silently relabelled.
    //
    // The guard is deliberately narrow: the projection is ABSENT, a table named
    // `session` carries the legacy SHAPE, and it holds at least one row. All
    // three are required, because only their conjunction can lose data.
    //
    // A migrated database still has its `session` table but reaches the
    // `verify_schema` arm above. A genuinely fresh Rust store has no `session`
    // table at all. A one-column `session` stub is not a legacy source — the
    // import would refuse it for missing columns, so refusing to OPEN over it
    // would strand a database the migration could never fix either. And an
    // empty legacy table has nothing to lose, so it is admitted rather than
    // refused; that boundary is pinned by its own witness so it is a stated
    // decision rather than an accident of the predicate.
    if legacy_source_is_populated(tx)? {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(
                "session-list projection is absent beside a legacy session source; \
                 the database must pass through ProductionMigrations rather than \
                 open an empty-rust-store projection over legacy rows"
                    .to_owned(),
            ),
        ));
    }
    create_schema(tx)?;
    tx.execute(
        "INSERT INTO session_list_import_meta
            (singleton, source_domain, source_rows, domain_digest)
         VALUES (1, 'empty-rust-store', 0, ?1)",
        [EMPTY_SHA256],
    )?;
    Ok(())
}

pub(crate) fn create_schema(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE session_list_projection (
            session_id TEXT PRIMARY KEY COLLATE BINARY,
            project_id TEXT NOT NULL COLLATE BINARY,
            workspace_id TEXT COLLATE BINARY,
            parent_id TEXT COLLATE BINARY,
            directory TEXT NOT NULL COLLATE BINARY,
            title TEXT NOT NULL,
            updated_ms INTEGER NOT NULL CHECK(typeof(updated_ms) = 'integer' AND updated_ms >= 0),
            provenance_kind TEXT NOT NULL CHECK(provenance_kind IN ('legacy-baseline', 'rust-event')),
            provenance_digest TEXT NOT NULL CHECK(length(provenance_digest) > 0),
            cutover_seq INTEGER NOT NULL CHECK(cutover_seq >= -1)
        );
        CREATE TABLE session_list_legacy_baseline (
            session_id TEXT PRIMARY KEY COLLATE BINARY,
            updated_ms INTEGER NOT NULL CHECK(typeof(updated_ms) = 'integer' AND updated_ms >= 0),
            source_domain TEXT NOT NULL,
            source_digest TEXT NOT NULL CHECK(length(source_digest) > 0),
            cutover_seq INTEGER NOT NULL CHECK(cutover_seq >= -1)
        );
        CREATE TABLE session_list_import_meta (
            singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
            source_domain TEXT NOT NULL,
            source_rows INTEGER NOT NULL CHECK(source_rows >= 0),
            domain_digest TEXT NOT NULL CHECK(length(domain_digest) > 0)
        );
        CREATE INDEX session_list_updated_key_idx
            ON session_list_projection (updated_ms, session_id);
        CREATE INDEX session_list_directory_key_idx
            ON session_list_projection (directory, updated_ms, session_id);
        CREATE INDEX session_list_project_key_idx
            ON session_list_projection (project_id, updated_ms, session_id);
        CREATE INDEX session_list_workspace_key_idx
            ON session_list_projection (workspace_id, updated_ms, session_id);
        CREATE INDEX session_list_parent_key_idx
            ON session_list_projection (parent_id, updated_ms, session_id);
        CREATE TRIGGER session_list_baseline_immutable_update
            BEFORE UPDATE ON session_list_legacy_baseline
            BEGIN SELECT RAISE(ABORT, 'session-list legacy baseline is immutable'); END;
        CREATE TRIGGER session_list_baseline_immutable_delete
            BEFORE DELETE ON session_list_legacy_baseline
            BEGIN SELECT RAISE(ABORT, 'session-list legacy baseline is immutable'); END;
        CREATE TRIGGER session_list_meta_immutable_update
            BEFORE UPDATE ON session_list_import_meta
            BEGIN SELECT RAISE(ABORT, 'session-list import provenance is immutable'); END;
        CREATE TRIGGER session_list_meta_immutable_delete
            BEFORE DELETE ON session_list_import_meta
            BEGIN SELECT RAISE(ABORT, 'session-list import provenance is immutable'); END;",
    )
}

/// Whole-projection verification: the O(1) shape check followed by the
/// O(rows) integrity scan.
///
/// Migrations require it before they build on an existing projection. Opening
/// a store requires only [`verify_schema_shape`]; list pages validate every
/// row they read and skip the ones that fail.
pub fn verify_schema(conn: &Connection) -> Result<(), SessionListSchemaError> {
    verify_schema_shape(conn)?;
    verify_projection_rows(conn)
}

/// The bounded, row-count-independent half of verification: required tables,
/// the exact projection column list, every required composite keyset index and
/// its BINARY collation, and the import-provenance singleton.
///
/// Every one of these reads is `PRAGMA`- or singleton-shaped, so its cost does
/// not grow with the projection. That is precisely why a list REQUEST may run
/// it inside its declared SQLite VM-step budget while the whole-table integrity
/// scan may not: the scan is `LIMIT 1` over a WHERE that no healthy row
/// satisfies, so on a clean projection it reads every row and its cost grows
/// without bound. Running it per request would make a lane that promises
/// bounded keyset pagination over a million imported rows refuse every page at
/// its own declared maximum import size.
pub fn verify_schema_shape(conn: &Connection) -> Result<(), SessionListSchemaError> {
    for table in [
        "session_list_projection",
        "session_list_legacy_baseline",
        "session_list_import_meta",
    ] {
        if !table_exists(conn, table)? {
            return Err(SessionListSchemaError::Unavailable);
        }
    }
    let required_columns = [
        "session_id",
        "project_id",
        "workspace_id",
        "parent_id",
        "directory",
        "title",
        "updated_ms",
        "provenance_kind",
        "provenance_digest",
        "cutover_seq",
    ];
    let actual = columns_of(conn, "session_list_projection")?;
    if actual != required_columns {
        return Err(SessionListSchemaError::Corrupt(format!(
            "session_list_projection columns are {actual:?}, expected {required_columns:?}"
        )));
    }
    for (name, columns) in [
        (
            "session_list_updated_key_idx",
            &["updated_ms", "session_id"][..],
        ),
        (
            "session_list_directory_key_idx",
            &["directory", "updated_ms", "session_id"][..],
        ),
        (
            "session_list_project_key_idx",
            &["project_id", "updated_ms", "session_id"][..],
        ),
        (
            "session_list_workspace_key_idx",
            &["workspace_id", "updated_ms", "session_id"][..],
        ),
        (
            "session_list_parent_key_idx",
            &["parent_id", "updated_ms", "session_id"][..],
        ),
    ] {
        verify_index(conn, name, columns)?;
    }
    // A missing or malformed provenance singleton IS corruption, but any other
    // SQLite failure here is not: mapping every error to `Corrupt` would report
    // an INTERRUPTED read — the shape a step-budget exhaustion takes — as a
    // corrupt projection. Accusing a healthy database of corruption because a
    // budget ran out is a false verdict, so the two are separated and only the
    // absent row is corruption.
    let meta: (i64, String, i64, String) = conn
        .query_row(
            "SELECT singleton, source_domain, source_rows, domain_digest
             FROM session_list_import_meta",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => {
                SessionListSchemaError::Corrupt("import provenance singleton is missing".to_owned())
            }
            other => SessionListSchemaError::Sqlite(other),
        })?;
    if meta.0 != 1 || meta.1.trim().is_empty() || meta.2 < 0 || meta.3.trim().is_empty() {
        return Err(SessionListSchemaError::Corrupt(
            "invalid import provenance singleton".to_owned(),
        ));
    }
    Ok(())
}

/// The O(rows) half: one joined pass proving every projection row agrees with
/// its legacy baseline, its provenance kind, its cutover sequence and its
/// authoritative watermark.
///
/// It belongs to migration, not to open or to a page request. A bounded page
/// request validates every row it actually READS — types, identities, location,
/// provenance/baseline agreement and watermark-versus-head — while decoding it,
/// so moving this scan off the request path does not let a corrupt row reach a
/// caller. It changes which rows a single request pays to inspect: the ones it
/// returns, rather than the whole table.
pub fn verify_projection_rows(conn: &Connection) -> Result<(), SessionListSchemaError> {
    let corrupt_row: Option<i64> = conn
        .query_row(
            "SELECT projection.rowid
             FROM session_list_projection AS projection
             LEFT JOIN session_list_legacy_baseline AS baseline
               ON baseline.session_id = projection.session_id
             LEFT JOIN projection_watermark AS watermark
               ON watermark.projection = 'session-list-v1'
              AND watermark.aggregate_family = 'session'
              AND watermark.aggregate_id = projection.session_id
             LEFT JOIN event_sequence AS head
               ON head.aggregate_family = 'session'
              AND head.aggregate_id = projection.session_id
             WHERE typeof(projection.session_id) <> 'text'
                OR typeof(projection.project_id) <> 'text'
                OR (projection.workspace_id IS NOT NULL AND typeof(projection.workspace_id) <> 'text')
                OR (projection.parent_id IS NOT NULL AND typeof(projection.parent_id) <> 'text')
                OR typeof(projection.directory) <> 'text'
                OR typeof(projection.title) <> 'text'
                OR typeof(projection.updated_ms) <> 'integer'
                OR projection.updated_ms < 0
                OR typeof(projection.provenance_digest) <> 'text'
                OR length(projection.provenance_digest) = 0
                OR projection.cutover_seq < -1
                OR (projection.provenance_kind = 'legacy-baseline' AND (
                    baseline.session_id IS NULL
                    OR baseline.updated_ms <> projection.updated_ms
                    OR baseline.source_digest <> projection.provenance_digest
                    OR baseline.cutover_seq <> projection.cutover_seq
                ))
                OR (projection.provenance_kind = 'rust-event' AND projection.cutover_seq <> -1)
                OR projection.provenance_kind NOT IN ('legacy-baseline', 'rust-event')
                OR (projection.cutover_seq >= 0 AND watermark.seq IS NULL)
                OR (watermark.seq IS NOT NULL AND (
                    watermark.seq < projection.cutover_seq
                    OR head.seq IS NULL
                    OR watermark.seq > head.seq
                ))
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|error| {
            if error == rusqlite::Error::QueryReturnedNoRows {
                Ok(None)
            } else {
                Err(error)
            }
        })?;
    if let Some(rowid) = corrupt_row {
        return Err(SessionListSchemaError::Corrupt(format!(
            "session_list_projection rowid {rowid} violates provenance, type, or watermark law"
        )));
    }
    Ok(())
}
