use super::projection::{location_of, validate_location};
use super::request::{
    NonNegativeMs, ParentFilter, SessionListCriteria, SessionListDirection, SessionListError,
    SessionListItem, SessionListKey, SessionListOrder, SessionListPage, SessionListProvenance,
    SessionListScope,
};
#[expect(
    unused_imports,
    reason = "SC15's compile-valid replacement names this type in the extracted query scope"
)]
use grapt_store::session_list::SqliteVmBudget;
use grapt_store::session_list::{verify_schema_shape, SessionListSchemaError};
use grapt_types::{ProjectId, SessionId, WorkspaceId};
use rusqlite::types::{Type, Value as SqlValue, ValueRef};
use rusqlite::{Connection, OptionalExtension};

#[derive(Debug)]
pub struct SessionListStore<'conn> {
    store: &'conn mut grapt_store::EventStore,
}

impl<'conn> SessionListStore<'conn> {
    /// Every list request uses a successfully opened store and exclusively
    /// owns hook registration for its bounded synchronous read. It uses a
    /// connection belonging to an `EventStore` that opened successfully — and
    /// an open cannot succeed unless the whole-projection integrity invariant
    /// passed. That successful open IS the receipt this request path relies on
    /// in place of rescanning the table per page; a public raw-connection
    /// constructor would let a caller obtain a list view that never had one.
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
                    CASE WHEN length(CAST(project_id AS BLOB)) <= ?2 THEN project_id END,
                    CASE WHEN length(CAST(directory AS BLOB)) <= ?2 THEN directory END,
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
            let project = required_text(row, 0, "project_id")?;
            let project =
                ProjectId::new(project).ok_or_else(|| SessionListError::ProjectionCorrupt {
                    reason: "project_id is empty".to_owned(),
                })?;
            let directory = required_text(row, 1, "directory")?;
            let updated = integer(row, 2, "updated_ms")?;
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
    #[expect(clippy::too_many_lines, reason = "legacy")]
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

        let physical = match (criteria.order, criteria.direction) {
            (SessionListOrder::Ascending, SessionListDirection::Next)
            | (SessionListOrder::Descending, SessionListDirection::Previous) => {
                SessionListOrder::Ascending
            }
            (SessionListOrder::Descending, SessionListDirection::Next)
            | (SessionListOrder::Ascending, SessionListDirection::Previous) => {
                SessionListOrder::Descending
            }
        };
        let order_sql = match physical {
            SessionListOrder::Ascending => "ASC",
            SessionListOrder::Descending => "DESC",
        };
        let comparison = match physical {
            SessionListOrder::Ascending => ">",
            SessionListOrder::Descending => "<",
        };

        let mut sql = String::from(
            "SELECT
                CASE WHEN length(CAST(session_id AS BLOB)) > ?1
                       OR length(CAST(project_id AS BLOB)) > ?1
                       OR length(CAST(COALESCE(workspace_id, '') AS BLOB)) > ?1
                       OR length(CAST(COALESCE(parent_id, '') AS BLOB)) > ?1
                       OR length(CAST(directory AS BLOB)) > ?1
                       OR length(CAST(title AS BLOB)) > ?1
                       OR length(CAST(provenance_kind AS BLOB)) > ?1
                       OR length(CAST(provenance_digest AS BLOB)) > ?1
                     THEN 1 ELSE 0 END,
                CASE WHEN length(CAST(session_id AS BLOB)) <= ?1 THEN session_id END,
                CASE WHEN length(CAST(project_id AS BLOB)) <= ?1 THEN project_id END,
                CASE WHEN workspace_id IS NULL OR length(CAST(workspace_id AS BLOB)) <= ?1
                     THEN workspace_id END,
                CASE WHEN parent_id IS NULL OR length(CAST(parent_id AS BLOB)) <= ?1
                     THEN parent_id END,
                CASE WHEN length(CAST(directory AS BLOB)) <= ?1 THEN directory END,
                CASE WHEN length(CAST(title AS BLOB)) <= ?1 THEN title END,
                updated_ms,
                provenance_kind,
                provenance_digest,
                cutover_seq,
                (SELECT seq FROM projection_watermark
                 WHERE projection = 'session-list-v1'
                   AND aggregate_family = 'session'
                   AND aggregate_id = session_list_projection.session_id),
                (SELECT seq FROM event_sequence
                 WHERE aggregate_family = 'session'
                   AND aggregate_id = session_list_projection.session_id)
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
                    " AND grapt_session_directory(directory) COLLATE BINARY = {parameter}"
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
            sql.push_str(&format!(" AND instr(title, {parameter}) > 0"));
        }
        match &criteria.parent {
            ParentFilter::Any => {}
            ParentFilter::Root => sql.push_str(" AND parent_id IS NULL"),
            ParentFilter::Child(parent) => {
                let parameter = push(SqlValue::Text(parent.as_str().to_owned()));
                sql.push_str(&format!(" AND parent_id COLLATE BINARY = {parameter}"));
            }
        }
        if let Some(anchor) = &criteria.anchor {
            let time = push(SqlValue::Integer(anchor.updated_ms.value()));
            let id = push(SqlValue::Text(anchor.session_id.as_str().to_owned()));
            sql.push_str(&format!(
                " AND (updated_ms {comparison} {time}
                   OR (updated_ms = {time} AND session_id COLLATE BINARY {comparison} {id}))"
            ));
        }
        let limit = push(SqlValue::Integer(
            i64::try_from(criteria.page_size.get()).map_err(|_| {
                SessionListError::InvalidCriteria {
                    reason: "page size does not fit SQLite INTEGER".to_owned(),
                }
            })?,
        ));
        sql.push_str(&format!(
            " ORDER BY updated_ms {order_sql}, session_id COLLATE BINARY {order_sql} LIMIT {limit}"
        ));

        self.read_page(&sql, params, criteria)
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
                move |context| {
                    let directory = context.get_raw(0).as_str()?;
                    if directory.len() > max_bytes {
                        return Err(rusqlite::Error::UserFunctionError(
                            "session directory exceeds the row text bound".into(),
                        ));
                    }
                    location_of(directory, None)
                        .map(|location| location.directory().to_owned())
                        .map_err(|reason| rusqlite::Error::UserFunctionError(reason.into()))
                },
            )
            .map_err(|source| SessionListError::StoreFailure { source })
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn read_page(
        &self,
        sql: &str,
        params: Vec<SqlValue>,
        criteria: &SessionListCriteria,
    ) -> Result<SessionListPage, SessionListError> {
        let mut statement = self
            .conn
            .prepare(sql)
            .map_err(|source| SessionListError::StoreFailure { source })?;
        let mut rows = statement
            .query(rusqlite::params_from_iter(params))
            .map_err(|source| SessionListError::StoreFailure { source })?;
        let mut items = Vec::with_capacity(criteria.page_size.get());
        let mut page_bytes = 0usize;
        while let Some(row) = rows
            .next()
            .map_err(|source| SessionListError::StoreFailure { source })?
        {
            let oversized: i64 = row
                .get(0)
                .map_err(|error| corrupt_sql("oversized marker", error))?;
            if oversized != 0 {
                return Err(SessionListError::ProjectionCorrupt {
                    reason: "a selected row contains text over the configured row bound".to_owned(),
                });
            }
            let session_raw = required_text(row, 1, "session_id")?;
            let project_raw = required_text(row, 2, "project_id")?;
            let workspace_raw: Option<String> = row
                .get(3)
                .map_err(|error| corrupt_sql("workspace_id", error))?;
            let parent_raw: Option<String> = row
                .get(4)
                .map_err(|error| corrupt_sql("parent_id", error))?;
            let directory = required_text(row, 5, "directory")?;
            let title = required_text(row, 6, "title")?;
            let updated_raw = integer(row, 7, "updated_ms")?;
            let provenance_kind = required_text(row, 8, "provenance_kind")?;
            let digest = required_text(row, 9, "provenance_digest")?;
            let cutover_seq = integer(row, 10, "cutover_seq")?;
            let watermark: Option<i64> = row
                .get(11)
                .map_err(|error| corrupt_sql("projection watermark", error))?;
            let head: Option<i64> = row
                .get(12)
                .map_err(|error| corrupt_sql("aggregate head", error))?;
            validate_watermark(&session_raw, cutover_seq, watermark, head)?;

            let session_id = session_raw.parse::<SessionId>().map_err(|error| {
                SessionListError::ProjectionCorrupt {
                    reason: format!("session_id {session_raw:?}: {error}"),
                }
            })?;
            let project_id = ProjectId::new(project_raw.clone()).ok_or_else(|| {
                SessionListError::ProjectionCorrupt {
                    reason: format!("session {session_raw} has an empty project_id"),
                }
            })?;
            let workspace_id = workspace_raw
                .map(|raw| {
                    WorkspaceId::new(raw).ok_or_else(|| SessionListError::ProjectionCorrupt {
                        reason: format!("session {session_raw} has an empty workspace_id"),
                    })
                })
                .transpose()?;
            validate_location(&directory, workspace_id.as_ref()).map_err(|reason| {
                SessionListError::ProjectionCorrupt {
                    reason: format!("session {session_raw} location: {reason}"),
                }
            })?;
            let parent_id = parent_raw
                .map(|raw| {
                    raw.parse::<SessionId>()
                        .map_err(|error| SessionListError::ProjectionCorrupt {
                            reason: format!("session {session_raw} parent_id: {error}"),
                        })
                })
                .transpose()?;
            let updated_ms = NonNegativeMs::new(updated_raw).ok_or_else(|| {
                SessionListError::ProjectionCorrupt {
                    reason: format!("session {session_raw} has negative updated_ms"),
                }
            })?;
            if digest.trim().is_empty() {
                return Err(SessionListError::ProjectionCorrupt {
                    reason: format!("session {session_raw} has blank provenance digest"),
                });
            }
            let provenance = match provenance_kind.as_str() {
                "legacy-baseline" if cutover_seq >= -1 => {
                    let baseline: Option<(i64, String, i64)> = self
                        .conn
                        .query_row(
                            "SELECT updated_ms, source_digest, cutover_seq
                             FROM session_list_legacy_baseline WHERE session_id = ?1",
                            [&session_raw],
                            |baseline| Ok((baseline.get(0)?, baseline.get(1)?, baseline.get(2)?)),
                        )
                        .optional()
                        .map_err(|source| SessionListError::StoreFailure { source })?;
                    if baseline != Some((updated_raw, digest.clone(), cutover_seq)) {
                        return Err(SessionListError::ProjectionCorrupt {
                            reason: format!(
                                "session {session_raw} baseline provenance does not match"
                            ),
                        });
                    }
                    SessionListProvenance::LegacyBaseline {
                        digest,
                        cutover_seq,
                    }
                }
                "rust-event" if cutover_seq == -1 => SessionListProvenance::RustEvent { digest },
                other => {
                    return Err(SessionListError::ProjectionCorrupt {
                        reason: format!(
                            "session {session_raw} has invalid provenance {other:?}/{cutover_seq}"
                        ),
                    })
                }
            };
            let row_bytes = session_raw.len()
                + project_raw.len()
                + workspace_id
                    .as_ref()
                    .map_or(0, |value| value.as_str().len())
                + parent_id.as_ref().map_or(0, |value| value.as_str().len())
                + directory.len()
                + title.len()
                + provenance_kind.len()
                + match &provenance {
                    SessionListProvenance::LegacyBaseline { digest, .. }
                    | SessionListProvenance::RustEvent { digest } => digest.len(),
                };
            page_bytes = page_bytes.checked_add(row_bytes).ok_or_else(|| {
                SessionListError::ProjectionCorrupt {
                    reason: "decoded page byte count overflowed".to_owned(),
                }
            })?;
            if page_bytes > criteria.limits.page_text_bytes.get() {
                return Err(SessionListError::ProjectionCorrupt {
                    reason: format!(
                        "decoded page text is {page_bytes} bytes; maximum is {}",
                        criteria.limits.page_text_bytes
                    ),
                });
            }
            items.push(SessionListItem {
                key: SessionListKey::new(updated_ms, session_id),
                project_id,
                workspace_id,
                parent_id,
                directory,
                title,
                provenance,
            });
        }
        if criteria.direction == SessionListDirection::Previous {
            items.reverse();
        }
        Ok(SessionListPage { items })
    }
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

fn corrupt_sql(field: &'static str, error: rusqlite::Error) -> SessionListError {
    SessionListError::ProjectionCorrupt {
        reason: format!("{field} has the wrong SQLite type: {error}"),
    }
}

fn required_text(
    row: &rusqlite::Row<'_>,
    index: usize,
    field: &'static str,
) -> Result<String, SessionListError> {
    row.get::<_, Option<String>>(index)
        .map_err(|error| corrupt_sql(field, error))?
        .ok_or_else(|| SessionListError::ProjectionCorrupt {
            reason: format!("{field} is absent or oversized"),
        })
}

fn integer(
    row: &rusqlite::Row<'_>,
    index: usize,
    field: &'static str,
) -> Result<i64, SessionListError> {
    let value = row
        .get_ref(index)
        .map_err(|error| corrupt_sql(field, error))?;
    if value.data_type() != Type::Integer {
        return Err(SessionListError::ProjectionCorrupt {
            reason: format!(
                "{field} has SQLite type {:?}, expected INTEGER",
                value.data_type()
            ),
        });
    }
    match value {
        ValueRef::Integer(value) => Ok(value),
        _ => Err(SessionListError::ProjectionCorrupt {
            reason: format!("{field} changed SQLite type while decoding"),
        }),
    }
}

fn validate_watermark(
    session: &str,
    cutover: i64,
    watermark: Option<i64>,
    head: Option<i64>,
) -> Result<(), SessionListError> {
    if cutover < -1 {
        return Err(SessionListError::ProjectionCorrupt {
            reason: format!("session {session} has invalid cutover {cutover}"),
        });
    }
    if cutover >= 0 && watermark.is_none() {
        return Err(SessionListError::ProjectionCorrupt {
            reason: format!("session {session} is missing its projection watermark"),
        });
    }
    if let Some(watermark) = watermark {
        if watermark < cutover || head.is_none_or(|head| watermark > head) {
            return Err(SessionListError::ProjectionCorrupt {
                reason: format!(
                    "session {session} watermark {watermark} is outside cutover/head {cutover}/{head:?}"
                ),
            });
        }
    }
    Ok(())
}
