use grapt_types::{LocationRef, Platform, ProjectId, SessionId, WorkspaceId};
use rusqlite::{Connection, Transaction};
use sha2::{Digest, Sha256};

use super::schema::{
    columns_of, create_schema, table_exists, verify_schema, LEGACY_SESSION_COLUMNS,
};
use super::{
    EMPTY_SHA256, IMPORT_BATCH_ROWS, LEGACY_SOURCE, MAX_IMPORTED_ROWS, MAX_ROW_TEXT_BYTES,
    SESSION_LIST_PROJECTION,
};

pub(crate) fn validate_legacy_source(conn: &Connection) -> Result<(), String> {
    if table_exists(conn, "session_list_projection").map_err(|error| error.to_string())? {
        return verify_schema(conn).map_err(|error| error.to_string());
    }
    if !table_exists(conn, "session").map_err(|error| error.to_string())? {
        return Ok(());
    }
    let columns = columns_of(conn, "session").map_err(|error| error.to_string())?;
    for column in LEGACY_SESSION_COLUMNS {
        if !columns.iter().any(|present| present == column) {
            return Err(format!("legacy session source is missing {column}"));
        }
    }
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM session", [], |row| row.get(0))
        .map_err(|error| error.to_string())?;
    let count = usize::try_from(count).map_err(|_| "negative legacy session count".to_owned())?;
    if count > MAX_IMPORTED_ROWS {
        return Err(format!(
            "legacy session source has {count} rows; maximum is {MAX_IMPORTED_ROWS}"
        ));
    }
    let corrupt: Option<i64> = conn
        .query_row(
            "SELECT rowid FROM session
             WHERE typeof(id) <> 'text'
                OR typeof(project_id) <> 'text'
                OR (workspace_id IS NOT NULL AND typeof(workspace_id) <> 'text')
                OR (parent_id IS NOT NULL AND typeof(parent_id) <> 'text')
                OR typeof(directory) <> 'text'
                OR typeof(title) <> 'text'
                OR typeof(time_updated) <> 'integer'
                OR length(CAST(id AS BLOB)) > ?1
                OR length(CAST(project_id AS BLOB)) > ?1
                OR length(CAST(COALESCE(workspace_id, '') AS BLOB)) > ?1
                OR length(CAST(COALESCE(parent_id, '') AS BLOB)) > ?1
                OR length(CAST(directory AS BLOB)) > ?1
                OR length(CAST(title AS BLOB)) > ?1
             LIMIT 1",
            [i64::try_from(MAX_ROW_TEXT_BYTES)
                .map_err(|_| "row byte bound overflow".to_owned())?],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|error| {
            if error == rusqlite::Error::QueryReturnedNoRows {
                Ok(None)
            } else {
                Err(error)
            }
        })
        .map_err(|error| error.to_string())?;
    if let Some(rowid) = corrupt {
        return Err(format!(
            "legacy session row {rowid} is corrupt or oversized"
        ));
    }
    scan_legacy(conn, |_| Ok(())).map(|_| ())
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(crate) fn import_legacy(tx: &Transaction<'_>) -> Result<(), String> {
    if table_exists(tx, "session_list_projection").map_err(|error| error.to_string())? {
        return verify_schema(tx).map_err(|error| error.to_string());
    }
    create_schema(tx).map_err(|error| error.to_string())?;
    if !table_exists(tx, "session").map_err(|error| error.to_string())? {
        tx.execute(
            "INSERT INTO session_list_import_meta
                (singleton, source_domain, source_rows, domain_digest)
             VALUES (1, 'empty-rust-store', 0, ?1)",
            [EMPTY_SHA256],
        )
        .map_err(|error| error.to_string())?;
        return Ok(());
    }

    let (rows, digest) = scan_legacy(tx, |_| Ok(()))?;
    let mut last: Option<String> = None;
    loop {
        // The continuation predicate and the batch ORDER BY must be the SAME
        // total order. `ORDER BY id COLLATE BINARY` is explicit, but a bare
        // `id > ?1` takes the COLUMN's declared collation — and the shipped
        // legacy `session.id` may declare NOCASE. The two then disagree at the
        // batch boundary: the ordering places `ses_A511` before `ses_a000`,
        // while a NOCASE `>` reports `ses_a000` as NOT greater, so the first
        // row of the second batch is skipped and the loop terminates one row
        // early. That is silent data loss behind a successful migration, so
        // the comparison carries its own explicit BINARY collation. An
        // explicit COLLATE on an operand overrides the column's declared
        // collation, which is exactly what makes the two agree.
        let mut statement = tx
            .prepare(
                "SELECT id, project_id, workspace_id, parent_id, directory, title, time_updated
                 FROM session
                 WHERE (?1 IS NULL OR id > ?1 COLLATE BINARY)
                 ORDER BY id COLLATE BINARY
                 LIMIT ?2",
            )
            .map_err(|error| error.to_string())?;
        let batch_limit = i64::try_from(IMPORT_BATCH_ROWS)
            .map_err(|_| "import batch bound overflow".to_owned())?;
        let mapped = statement
            .query_map(rusqlite::params![last.as_deref(), batch_limit], |row| {
                Ok(LegacyRow {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    workspace_id: row.get(2)?,
                    parent_id: row.get(3)?,
                    directory: row.get(4)?,
                    title: row.get(5)?,
                    updated_ms: row.get(6)?,
                })
            })
            .map_err(|error| error.to_string())?;
        let batch = mapped
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        drop(statement);
        if batch.is_empty() {
            break;
        }
        for row in &batch {
            validate_row(row)?;
            let cutover_seq: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(seq), -1) FROM event
                     WHERE aggregate_family = 'session' AND aggregate_id = ?1",
                    [&row.id],
                    |result| result.get(0),
                )
                .map_err(|error| error.to_string())?;
            tx.execute(
                "INSERT INTO session_list_legacy_baseline
                    (session_id, updated_ms, source_domain, source_digest, cutover_seq)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![row.id, row.updated_ms, LEGACY_SOURCE, digest, cutover_seq],
            )
            .map_err(|error| error.to_string())?;
            tx.execute(
                "INSERT INTO session_list_projection
                    (session_id, project_id, workspace_id, parent_id, directory, title,
                     updated_ms, provenance_kind, provenance_digest, cutover_seq)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'legacy-baseline', ?8, ?9)",
                rusqlite::params![
                    row.id,
                    row.project_id,
                    row.workspace_id,
                    row.parent_id,
                    row.directory,
                    row.title,
                    row.updated_ms,
                    digest,
                    cutover_seq
                ],
            )
            .map_err(|error| error.to_string())?;
            if cutover_seq >= 0 {
                tx.execute(
                    "INSERT INTO projection_watermark
                        (projection, aggregate_family, aggregate_id, seq)
                     VALUES (?1, 'session', ?2, ?3)",
                    rusqlite::params![SESSION_LIST_PROJECTION.as_str(), row.id, cutover_seq],
                )
                .map_err(|error| error.to_string())?;
            }
        }
        last = batch.last().map(|row| row.id.clone());
    }
    // Bounded import postcondition. The batch loop is the only writer, but a
    // successful loop is NOT evidence that the domain arrived: a continuation
    // predicate that disagrees with its ordering, a batch bound that stops
    // early, or a filtered read all terminate normally having written fewer
    // rows than the source holds. Recomputing the projected count and domain
    // digest and comparing them against the values recorded by the read-only
    // preflight is what turns a partial import into an honest atomic failure
    // instead of a successful migration over missing data. This runs BEFORE
    // the provenance row is written, so a database can never carry an import
    // receipt describing a domain it does not contain; returning `Err` here
    // aborts the migration transaction and the whole import rolls back.
    verify_import_postcondition(tx, rows, &digest)?;
    tx.execute(
        "INSERT INTO session_list_import_meta
            (singleton, source_domain, source_rows, domain_digest)
         VALUES (1, ?1, ?2, ?3)",
        rusqlite::params![
            LEGACY_SOURCE,
            i64::try_from(rows).map_err(|_| "legacy row count overflow".to_owned())?,
            digest
        ],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

#[derive(Debug)]
pub(super) struct LegacyRow {
    pub(super) id: String,
    pub(super) project_id: String,
    pub(super) workspace_id: Option<String>,
    pub(super) parent_id: Option<String>,
    pub(super) directory: String,
    pub(super) title: String,
    pub(super) updated_ms: i64,
}

fn scan_legacy(
    conn: &Connection,
    mut observe: impl FnMut(&LegacyRow) -> Result<(), String>,
) -> Result<(usize, String), String> {
    let mut statement = conn
        .prepare(
            "SELECT id, project_id, workspace_id, parent_id, directory, title, time_updated
             FROM session ORDER BY id COLLATE BINARY",
        )
        .map_err(|error| error.to_string())?;
    let mapped = statement
        .query_map([], |row| {
            Ok(LegacyRow {
                id: row.get(0)?,
                project_id: row.get(1)?,
                workspace_id: row.get(2)?,
                parent_id: row.get(3)?,
                directory: row.get(4)?,
                title: row.get(5)?,
                updated_ms: row.get(6)?,
            })
        })
        .map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut count = 0usize;
    for row in mapped {
        let row = row.map_err(|error| error.to_string())?;
        validate_row(&row)?;
        observe(&row)?;
        feed_row(&mut hasher, &row);
        count = count
            .checked_add(1)
            .ok_or_else(|| "legacy row count overflow".to_owned())?;
        if count > MAX_IMPORTED_ROWS {
            return Err(format!(
                "legacy session source exceeds {MAX_IMPORTED_ROWS} rows"
            ));
        }
    }
    Ok((count, format!("{:x}", hasher.finalize())))
}

/// The exact per-row domain encoding. Source and projection MUST hash through
/// this one function: two hand-written copies of the field order would let the
/// postcondition compare digests that were never computed the same way, which
/// is a comparison that can only fail for the wrong reason or pass for it.
pub(super) fn feed_row(hasher: &mut Sha256, row: &LegacyRow) {
    feed(hasher, Some(&row.id));
    feed(hasher, Some(&row.project_id));
    feed(hasher, row.workspace_id.as_deref());
    feed(hasher, row.parent_id.as_deref());
    feed(hasher, Some(&row.directory));
    feed(hasher, Some(&row.title));
    hasher.update(row.updated_ms.to_be_bytes());
}

/// Compares the projected domain against the domain the read-only preflight
/// recorded over the legacy source.
///
/// Extracted so the postcondition has a witness that does not require a live
/// import defect to reach: with the batch reader correct, no ordinary import
/// can produce the mismatch this guards against, so a postcondition reachable
/// only through `import_legacy` would be an assertion nothing could ever
/// exercise. A guard no test can reach is decorative.
pub(super) fn verify_import_postcondition(
    conn: &Connection,
    expected_rows: usize,
    expected_digest: &str,
) -> Result<(), String> {
    let (projected_rows, projected_digest) = projected_domain(conn)?;
    if projected_rows != expected_rows || projected_digest != expected_digest {
        return Err(format!(
            "session-list import postcondition failed: legacy source recorded {expected_rows} \
             rows with domain digest {expected_digest}, projection holds {projected_rows} rows \
             with domain digest {projected_digest}"
        ));
    }
    Ok(())
}

/// Recomputes the imported domain from the PROJECTION, in the same total order
/// and the same encoding the read-only preflight used over the source.
pub(super) fn projected_domain(conn: &Connection) -> Result<(usize, String), String> {
    let mut statement = conn
        .prepare(
            "SELECT session_id, project_id, workspace_id, parent_id, directory, title, updated_ms
             FROM session_list_projection
             ORDER BY session_id COLLATE BINARY",
        )
        .map_err(|error| error.to_string())?;
    let mapped = statement
        .query_map([], |row| {
            Ok(LegacyRow {
                id: row.get(0)?,
                project_id: row.get(1)?,
                workspace_id: row.get(2)?,
                parent_id: row.get(3)?,
                directory: row.get(4)?,
                title: row.get(5)?,
                updated_ms: row.get(6)?,
            })
        })
        .map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut count = 0usize;
    for row in mapped {
        let row = row.map_err(|error| error.to_string())?;
        feed_row(&mut hasher, &row);
        count = count
            .checked_add(1)
            .ok_or_else(|| "projected row count overflow".to_owned())?;
    }
    Ok((count, format!("{:x}", hasher.finalize())))
}

fn feed(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        None => hasher.update([0]),
        Some(value) => {
            hasher.update([1]);
            hasher.update((value.len() as u64).to_be_bytes());
            hasher.update(value.as_bytes());
        }
    }
}

fn validate_row(row: &LegacyRow) -> Result<(), String> {
    row.id
        .parse::<SessionId>()
        .map_err(|error| format!("legacy session identity {}: {error}", row.id))?;
    ProjectId::new(row.project_id.clone())
        .ok_or_else(|| format!("legacy session {} has an empty project identity", row.id))?;
    if let Some(workspace) = &row.workspace_id {
        WorkspaceId::new(workspace.clone())
            .ok_or_else(|| format!("legacy session {} has an empty workspace identity", row.id))?;
    }
    if let Some(parent) = &row.parent_id {
        parent
            .parse::<SessionId>()
            .map_err(|error| format!("legacy session {} parent identity: {error}", row.id))?;
    }
    let platform = if row.directory.starts_with('/') {
        Platform::Posix
    } else {
        Platform::Windows
    };
    LocationRef::on(platform, &row.directory, None)
        .map_err(|error| format!("legacy session {} directory: {error}", row.id))?;
    if row.updated_ms < 0 {
        return Err(format!("legacy session {} has negative updated_ms", row.id));
    }
    Ok(())
}
