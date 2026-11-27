use super::keyset::{self, After, Segment};
use super::projection::location_of;
use super::request::{
    BoundaryKey, KeyDigest, ParentFilter, SessionListCriteria, SessionListDegradedRow,
    SessionListDirection, SessionListError, SessionListItem, SessionListOrder, SessionListPage,
    SessionListScope, MAX_PAGE_ROWS,
};
use super::row;
#[expect(
    unused_imports,
    reason = "SC15's compile-valid replacement names this type in the extracted query scope"
)]
use grapt_store::session_list::SqliteVmBudget;
use grapt_store::session_list::{verify_schema_shape, SessionListSchemaError};
use grapt_types::ProjectId;
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::Connection;

#[derive(Debug)]
pub struct SessionListStore<'conn> {
    store: &'conn mut grapt_store::EventStore,
}

impl<'conn> SessionListStore<'conn> {
    /// Every list request uses a successfully opened store and exclusively
    /// owns hook registration for its bounded synchronous read. A successful
    /// open proves the projection's shape; each page then validates every row
    /// it reads and skips, with a reason, any row that breaks the projection
    /// law. A public raw-connection constructor would let a caller obtain a
    /// list view whose shape was never proven.
    pub fn over(store: &'conn mut grapt_store::EventStore) -> Self {
        Self { store }
    }

    /// Serves one bounded page.
    ///
    /// The SQLite VM-step budget is installed BEFORE any SQLite work this
    /// request performs, so schema verification, the keyset query and every
    /// per-row decode read share one declared bound. Verification used to run
    /// ahead of the guard, which left an unbounded joined scan on the request
    /// path; the bound is now the first thing established and the last thing
    /// released.
    pub fn list(
        &mut self,
        criteria: &SessionListCriteria,
    ) -> Result<SessionListPage, SessionListError> {
        let guard = self
            .store
            .read_budget(criteria.limits.sqlite_vm_steps)
            .map_err(|source| SessionListError::StoreFailure { source })?;
        let result = Query {
            conn: guard.connection(),
        }
        .bounded_list(criteria);
        let exhausted = guard.exhausted();
        guard
            .finish()
            .map_err(|source| SessionListError::StoreFailure { source })?;
        match result {
            Err(SessionListError::StoreFailure { source })
                if exhausted && sqlite_interrupted(&source) =>
            {
                Err(SessionListError::QueryBudgetExceeded)
            }
            other => other,
        }
    }

    /// Where the listed Sessions live, newest first: each (project, directory)
    /// pair once, with the update time of its most recently updated Session.
    /// At most `limit` rows, under the production VM-step budget.
    pub fn recent_projects(
        &mut self,
        limit: usize,
    ) -> Result<Vec<(ProjectId, String, i64)>, SessionListError> {
        let limits = super::request::SessionListLimits::production();
        let guard = self
            .store
            .read_budget(limits.sqlite_vm_steps)
            .map_err(|source| SessionListError::StoreFailure { source })?;
        let result = Query {
            conn: guard.connection(),
        }
        .bounded_recent_projects(limit, &limits);
        let exhausted = guard.exhausted();
        guard
            .finish()
            .map_err(|source| SessionListError::StoreFailure { source })?;
        match result {
            Err(SessionListError::StoreFailure { source })
                if exhausted && sqlite_interrupted(&source) =>
            {
                Err(SessionListError::QueryBudgetExceeded)
            }
            other => other,
        }
    }
}

struct Query<'conn> {
    conn: &'conn Connection,
}

impl Query<'_> {
    fn bounded_recent_projects(
        &self,
        limit: usize,
        limits: &super::request::SessionListLimits,
    ) -> Result<Vec<(ProjectId, String, i64)>, SessionListError> {
        verify_schema_shape(self.conn).map_err(map_schema)?;
        let limit = i64::try_from(limit).map_err(|_| SessionListError::InvalidCriteria {
            reason: "project limit exceeds SQLite range".to_owned(),
        })?;
        let row_text = i64::try_from(limits.row_text_bytes.get()).map_err(|_| {
            SessionListError::InvalidCriteria {
                reason: "row text limit exceeds SQLite range".to_owned(),
            }
        })?;
        // As in a list page: text over the row bound decodes as NULL, which
        // required_text refuses, so no oversized value is ever materialized.
        let mut statement = self
            .conn
            .prepare(
                "SELECT
                    CASE WHEN octet_length(project_id) <= ?2 THEN project_id END,
                    CASE WHEN octet_length(directory) <= ?2 THEN directory END,
                    MAX(updated_ms) AS newest
                 FROM session_list_projection
                 GROUP BY project_id, directory
                 ORDER BY newest DESC, project_id, directory
                 LIMIT ?1",
            )
            .map_err(|source| SessionListError::StoreFailure { source })?;
        let mut rows = statement
            .query([limit, row_text])
            .map_err(|source| SessionListError::StoreFailure { source })?;
        let mut projects = Vec::new();
        let mut page_bytes = 0_usize;
        while let Some(row) = rows
            .next()
            .map_err(|source| SessionListError::StoreFailure { source })?
        {
            let project = row::required_text(row, 0, "project_id")?;
            let project =
                ProjectId::new(project).ok_or_else(|| SessionListError::ProjectionCorrupt {
                    reason: "project_id is empty".to_owned(),
                })?;
            let directory = row::required_text(row, 1, "directory")?;
            let updated = row::integer(row, 2, "updated_ms")?;
            page_bytes = page_bytes
                .checked_add(project.as_str().len() + directory.len())
                .ok_or_else(|| SessionListError::ProjectionCorrupt {
                    reason: "decoded project text byte count overflowed".to_owned(),
                })?;
            if page_bytes > limits.page_text_bytes.get() {
                return Err(SessionListError::ProjectionCorrupt {
                    reason: format!(
                        "decoded project text is {page_bytes} bytes; maximum is {}",
                        limits.page_text_bytes
                    ),
                });
            }
            projects.push((project, directory, updated));
        }
        Ok(projects)
    }

    /// Every SQLite read a list request makes lives here, under the caller's
    /// installed budget.
    ///
    /// Rows that break the projection law are skipped and listed in the
    /// page's degraded rows. When skipped rows leave the page short, reading
    /// continues after the last row read, so a run of corrupt rows never
    /// passes for the end of the list. At most [`MAX_PAGE_ROWS`] rows are
    /// skipped per page, and the VM-step budget bounds the whole request.
    fn bounded_list(
        &self,
        criteria: &SessionListCriteria,
    ) -> Result<SessionListPage, SessionListError> {
        verify_schema_shape(self.conn).map_err(map_schema)?;
        // Historical projections retain the original platform's spelling.
        // Compare through the same owner used by Session placement, without
        // rewriting durable evidence or treating POSIX backslashes as separators.
        if matches!(criteria.scope, SessionListScope::Directory(_)) {
            self.install_directory_function(criteria.limits.row_text_bytes.get())?;
        }
        let start = match &criteria.anchor {
            None => None,
            Some(anchor) => Some(match &anchor.after {
                BoundaryKey::Session(key) => After::Session {
                    time: key.updated_ms.value(),
                    id: key.session_id.as_str().to_owned(),
                },
                BoundaryKey::Row { rowid, key } => After::Row {
                    rowid: *rowid,
                    keyed: self.require_row(*rowid, *key)?,
                },
            }),
        };
        let descending = descending(criteria);
        let mut pending =
            std::collections::VecDeque::from(keyset::segments(start.as_ref(), descending));
        let mut page = PageFill::default();
        let mut continue_after = None;
        while let Some(segment) = pending.front() {
            let wanted = criteria.page_size.get() - page.items.len();
            let (sql, params) = page_sql(criteria, segment, wanted)?;
            let (scanned, last) = self.read_rows(&sql, params, criteria, &mut page)?;
            if page.full || page.items.len() == criteria.page_size.get() {
                break;
            }
            let exhausted = scanned < wanted;
            if exhausted && pending.len() == 1 {
                break;
            }
            if page.degraded.len() >= MAX_PAGE_ROWS {
                // Stopped by the skip bound, not the end of the list.
                continue_after = match last {
                    Some(After::Row { rowid, .. }) => row::boundary_after(self.conn, rowid)
                        .map_err(|source| SessionListError::StoreFailure { source })?,
                    _ => None,
                };
                break;
            }
            if exhausted {
                pending.pop_front();
            } else {
                // Skipped rows left the page short: read on after the last.
                let Some(last) = last else { break };
                pending = keyset::segments(Some(&last), descending).into();
            }
        }
        let PageFill {
            mut items,
            degraded,
            ..
        } = page;
        if criteria.direction == SessionListDirection::Previous {
            items.reverse();
        }
        Ok(SessionListPage {
            items,
            degraded,
            continue_after,
        })
    }

    /// A cursor after a stored row continues only while that row still holds
    /// the time and key it named; otherwise the rows that tie with it cannot
    /// be placed, and listing restarts instead of skipping them. Returns
    /// whether that key is not NULL.
    fn require_row(&self, rowid: i64, key: KeyDigest) -> Result<bool, SessionListError> {
        match row::stored_key(self.conn, rowid)
            .map_err(|source| SessionListError::StoreFailure { source })?
        {
            Some(stored) if stored.digest == key => Ok(stored.keyed),
            _ => Err(SessionListError::StaleCursor),
        }
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "this adapter registers only its fixed directory-query function, never a trigger dependency"
    )]
    fn install_directory_function(&self, max_bytes: usize) -> Result<(), SessionListError> {
        use rusqlite::functions::FunctionFlags;
        self.conn
            .create_scalar_function(
                "grapt_session_directory",
                1,
                FunctionFlags::SQLITE_UTF8
                    | FunctionFlags::SQLITE_DETERMINISTIC
                    | FunctionFlags::SQLITE_INNOCUOUS,
                // A directory that is not text, is over the row bound or is
                // not a location belongs to no directory: its row matches no
                // directory scope, and the unscoped list names it as corrupt.
                move |context| {
                    Ok(match context.get_raw(0) {
                        ValueRef::Text(bytes) if bytes.len() <= max_bytes => {
                            std::str::from_utf8(bytes).ok().and_then(|directory| {
                                location_of(directory, None)
                                    .ok()
                                    .map(|location| location.directory().to_owned())
                            })
                        }
                        _ => None,
                    })
                },
            )
            .map_err(|source| SessionListError::StoreFailure { source })
    }

    /// Reads one statement's rows into `page`. A row that breaks the
    /// projection law is skipped and reported, never allowed to fail the
    /// page. Returns how many rows the statement yielded and where the last
    /// one sits, so the caller can continue past skipped rows.
    fn read_rows(
        &self,
        sql: &str,
        params: Vec<SqlValue>,
        criteria: &SessionListCriteria,
        page: &mut PageFill,
    ) -> Result<(usize, Option<After>), SessionListError> {
        let mut statement = self
            .conn
            .prepare(sql)
            .map_err(|source| SessionListError::StoreFailure { source })?;
        let mut rows = statement
            .query(rusqlite::params_from_iter(params))
            .map_err(|source| SessionListError::StoreFailure { source })?;
        let (mut scanned, mut last) = (0, None);
        while let Some(row) = rows
            .next()
            .map_err(|source| SessionListError::StoreFailure { source })?
        {
            scanned += 1;
            last = row::after(row);
            match row::decode(self.conn, row) {
                Ok((item, bytes)) => {
                    let total = page.bytes.saturating_add(bytes);
                    if total > criteria.limits.page_text_bytes.get() && !page.items.is_empty() {
                        // The page is full by bytes: end it before this row,
                        // which the next page's cursor reads again.
                        page.full = true;
                        return Ok((scanned, last));
                    }
                    page.bytes = total;
                    page.items.push(item);
                }
                Err(SessionListError::ProjectionCorrupt { reason }) => {
                    page.degraded.push(SessionListDegradedRow {
                        session_id: row::session_hint(row),
                        reason,
                    });
                }
                Err(other) => return Err(other),
            }
        }
        Ok((scanned, last))
    }
}

/// A page being filled across one or more statements.
#[derive(Default)]
struct PageFill {
    items: Vec<SessionListItem>,
    degraded: Vec<SessionListDegradedRow>,
    bytes: usize,
    full: bool,
}

/// Whether the page reads the keyset from its high end.
fn descending(criteria: &SessionListCriteria) -> bool {
    matches!(
        (criteria.order, criteria.direction),
        (SessionListOrder::Descending, SessionListDirection::Next)
            | (SessionListOrder::Ascending, SessionListDirection::Previous)
    )
}

/// One statement for up to `limit` rows of `segment`, in the page's physical
/// order.
fn page_sql(
    criteria: &SessionListCriteria,
    segment: &Segment,
    limit: usize,
) -> Result<(String, Vec<SqlValue>), SessionListError> {
    let (order_sql, comparison) = if descending(criteria) {
        ("DESC", "<")
    } else {
        ("ASC", ">")
    };

    let mut sql = String::from(
        "SELECT
            CASE WHEN octet_length(session_id) > ?1
                   OR octet_length(project_id) > ?1
                   OR octet_length(workspace_id) > ?1
                   OR octet_length(parent_id) > ?1
                   OR octet_length(directory) > ?1
                   OR octet_length(title) > ?1
                   OR octet_length(provenance_kind) > ?1
                   OR octet_length(provenance_digest) > ?1
                 THEN 1 ELSE 0 END,
            CASE WHEN octet_length(session_id) <= ?1 THEN session_id END,
            CASE WHEN octet_length(project_id) <= ?1 THEN project_id END,
            CASE WHEN workspace_id IS NULL OR octet_length(workspace_id) <= ?1
                 THEN workspace_id END,
            CASE WHEN parent_id IS NULL OR octet_length(parent_id) <= ?1
                 THEN parent_id END,
            CASE WHEN octet_length(directory) <= ?1 THEN directory END,
            CASE WHEN octet_length(title) <= ?1 THEN title END,
            CASE WHEN typeof(updated_ms) = 'integer' THEN updated_ms END,
            CASE WHEN octet_length(provenance_kind) <= ?1 THEN provenance_kind END,
            CASE WHEN octet_length(provenance_digest) <= ?1 THEN provenance_digest END,
            CASE WHEN typeof(cutover_seq) = 'integer' THEN cutover_seq END,
            CASE WHEN octet_length(session_id) <= ?1 THEN
                (SELECT seq FROM projection_watermark
                 WHERE projection = 'session-list-v1'
                   AND aggregate_family = 'session'
                   AND aggregate_id = session_list_projection.session_id) END,
            CASE WHEN octet_length(session_id) <= ?1 THEN
                (SELECT seq FROM event_sequence
                 WHERE aggregate_family = 'session'
                   AND aggregate_id = session_list_projection.session_id) END,
            rowid,
            typeof(session_id) <> 'null'
         FROM session_list_projection WHERE 1 = 1",
    );
    let mut params = vec![SqlValue::Integer(
        i64::try_from(criteria.limits.row_text_bytes.get()).map_err(|_| {
            SessionListError::InvalidCriteria {
                reason: "row text limit does not fit SQLite INTEGER".to_owned(),
            }
        })?,
    )];
    let mut push = |value: SqlValue| {
        params.push(value);
        format!("?{}", params.len())
    };

    match &criteria.scope {
        SessionListScope::All => {}
        SessionListScope::Directory(location) => {
            let parameter = push(SqlValue::Text(location.directory().to_owned()));
            sql.push_str(&format!(
                " AND grapt_session_directory(CASE WHEN octet_length(directory) <= ?1
                   THEN directory END) COLLATE BINARY = {parameter}"
            ));
        }
        SessionListScope::Project(project) => {
            let parameter = push(SqlValue::Text(project.as_str().to_owned()));
            sql.push_str(&format!(" AND project_id COLLATE BINARY = {parameter}"));
        }
    }
    if let Some(workspace) = criteria.effective_workspace() {
        let parameter = push(SqlValue::Text(workspace.as_str().to_owned()));
        sql.push_str(&format!(" AND workspace_id COLLATE BINARY = {parameter}"));
    }
    if let Some(search) = &criteria.title_contains {
        let parameter = push(SqlValue::Text(search.as_str().to_owned()));
        // A title that is not text or is over the row bound is never read, so
        // it matches no search; the unscoped list names its row as corrupt.
        sql.push_str(&format!(
            " AND instr(CASE WHEN typeof(title) = 'text' AND octet_length(title) <= ?1
                   THEN title END, {parameter}) > 0"
        ));
    }
    match &criteria.parent {
        ParentFilter::Any => {}
        ParentFilter::Root => sql.push_str(" AND parent_id IS NULL"),
        ParentFilter::Child(parent) => {
            let parameter = push(SqlValue::Text(parent.as_str().to_owned()));
            sql.push_str(&format!(" AND parent_id COLLATE BINARY = {parameter}"));
        }
    }
    sql.push_str(&segment.terms(comparison, &mut push));
    let limit = push(SqlValue::Integer(i64::try_from(limit).map_err(|_| {
        SessionListError::InvalidCriteria {
            reason: "page size does not fit SQLite INTEGER".to_owned(),
        }
    })?));
    sql.push_str(&format!(
        // SQLite sorts NULL keys first ascending and last descending, the
        // keyset's own order, so the key index serves every segment directly.
        " ORDER BY updated_ms {order_sql}, session_id COLLATE BINARY {order_sql},
           rowid {order_sql} LIMIT {limit}"
    ));

    Ok((sql, params))
}

fn sqlite_interrupted(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::OperationInterrupted
    )
}

fn map_schema(error: SessionListSchemaError) -> SessionListError {
    match error {
        SessionListSchemaError::Unavailable => SessionListError::ProjectionUnavailable,
        SessionListSchemaError::Corrupt(reason) => SessionListError::ProjectionCorrupt { reason },
        SessionListSchemaError::Sqlite(source) => SessionListError::StoreFailure { source },
    }
}

#[cfg(test)]
mod plan_tests {
    use super::*;
    use crate::list::request::SessionListLimits;

    /// Every statement a page can run is one seek in key-index order: a
    /// sort or filtered scan over rows sharing a time would cost the whole
    /// run before the first row.
    #[test]
    fn every_segment_is_one_seek_in_index_order() -> Result<(), Box<dyn std::error::Error>> {
        let store = grapt_store::EventStore::open_in_memory()?;
        let positions = [
            None,
            Some(After::Session {
                time: 5,
                id: "ses_a".to_owned(),
            }),
            Some(After::Row {
                rowid: 1,
                keyed: true,
            }),
            Some(After::Row {
                rowid: 1,
                keyed: false,
            }),
        ];
        for order in [SessionListOrder::Ascending, SessionListOrder::Descending] {
            let criteria = SessionListCriteria::new(
                SessionListScope::All,
                None,
                None,
                ParentFilter::Any,
                order,
                SessionListDirection::Next,
                None,
                2,
                SessionListLimits::production(),
            )?;
            for position in &positions {
                for segment in keyset::segments(position.as_ref(), descending(&criteria)) {
                    let (sql, params) = page_sql(&criteria, &segment, 2)?;
                    let mut statement = store
                        .connection()
                        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
                    let plan = statement
                        .query_map(rusqlite::params_from_iter(params), |row| {
                            row.get::<_, String>(3)
                        })?
                        .collect::<Result<Vec<_>, _>>()?;
                    let outer = plan
                        .iter()
                        .find(|step| step.contains("session_list_projection"))
                        .ok_or("no projection step")?;
                    assert!(
                        !plan.iter().any(|step| step.contains("TEMP B-TREE"))
                            && (outer.starts_with("SEARCH") || segment == Segment::All),
                        "{order:?} {segment:?} is not one seek: {plan:?}"
                    );
                }
            }
        }
        Ok(())
    }
}
