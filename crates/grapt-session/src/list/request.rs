use grapt_store::session_list::{SqliteVmBudget, MAX_ROW_TEXT_BYTES};
use grapt_types::{LocationRef, ProjectId, SessionId, WorkspaceId};
use std::num::NonZeroUsize;

pub const MAX_PAGE_ROWS: usize = 128;
pub const MAX_SEARCH_UTF8_BYTES: usize = 1_024;
pub const MAX_PAGE_TEXT_BYTES: usize = 8_388_608;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct NonNegativeMs(i64);

impl NonNegativeMs {
    pub fn new(value: i64) -> Option<Self> {
        (value >= 0).then_some(Self(value))
    }

    pub const fn value(self) -> i64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionListKey {
    pub(super) updated_ms: NonNegativeMs,
    pub(super) session_id: SessionId,
}

impl SessionListKey {
    pub fn new(updated_ms: NonNegativeMs, session_id: SessionId) -> Self {
        Self {
            updated_ms,
            session_id,
        }
    }

    pub const fn updated_ms(&self) -> NonNegativeMs {
        self.updated_ms
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionListOrder {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionListDirection {
    Next,
    Previous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionListScope {
    All,
    Directory(LocationRef),
    Project(ProjectId),
}

impl SessionListScope {
    /// Compatibility adapter for callers that still carry the oracle's
    /// presentation-only `subpath`. Store ownership ends at the typed project;
    /// varying a display subpath must not alter persisted query criteria.
    pub fn project(project: ProjectId, compatibility_subpath: Option<&str>) -> Self {
        let _ = compatibility_subpath;
        Self::Project(project)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParentFilter {
    Any,
    Root,
    Child(SessionId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionListLimits {
    page_rows: NonZeroUsize,
    search_utf8_bytes: NonZeroUsize,
    pub(super) row_text_bytes: NonZeroUsize,
    pub(super) page_text_bytes: NonZeroUsize,
    pub(super) sqlite_vm_steps: SqliteVmBudget,
}

impl SessionListLimits {
    pub fn new(
        page_rows: usize,
        search_utf8_bytes: usize,
        row_text_bytes: usize,
        page_text_bytes: usize,
        sqlite_vm_steps: u64,
    ) -> Result<Self, SessionListError> {
        let positive = |name, value| {
            NonZeroUsize::new(value).ok_or(SessionListError::InvalidCriteria {
                reason: format!("{name} must be positive"),
            })
        };
        Ok(Self {
            page_rows: positive("page row limit", page_rows)?,
            search_utf8_bytes: positive("search byte limit", search_utf8_bytes)?,
            row_text_bytes: positive("row text limit", row_text_bytes)?,
            page_text_bytes: positive("page text limit", page_text_bytes)?,
            sqlite_vm_steps: SqliteVmBudget::new(sqlite_vm_steps).ok_or(
                SessionListError::InvalidCriteria {
                    reason: "SQLite VM-step budget must be positive".to_owned(),
                },
            )?,
        })
    }

    pub fn production() -> Self {
        match Self::new(
            MAX_PAGE_ROWS,
            MAX_SEARCH_UTF8_BYTES,
            MAX_ROW_TEXT_BYTES,
            MAX_PAGE_TEXT_BYTES,
            grapt_store::session_list::MAX_SQLITE_VM_STEPS,
        ) {
            Ok(value) => value,
            Err(_) => unreachable!("the production session-list limits are positive"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedSearch(String);

impl BoundedSearch {
    pub fn new(
        value: impl Into<String>,
        limits: SessionListLimits,
    ) -> Result<Self, SessionListError> {
        let value = value.into();
        if value.len() > limits.search_utf8_bytes.get() {
            return Err(SessionListError::SearchLimitExceeded {
                actual: value.len(),
                maximum: limits.search_utf8_bytes.get(),
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionListCriteria {
    pub scope: SessionListScope,
    pub workspace: Option<WorkspaceId>,
    pub title_contains: Option<BoundedSearch>,
    pub parent: ParentFilter,
    pub order: SessionListOrder,
    pub direction: SessionListDirection,
    pub anchor: Option<SessionListKey>,
    pub(super) page_size: NonZeroUsize,
    pub(super) limits: SessionListLimits,
}

impl SessionListCriteria {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        scope: SessionListScope,
        workspace: Option<WorkspaceId>,
        title_contains: Option<BoundedSearch>,
        parent: ParentFilter,
        order: SessionListOrder,
        direction: SessionListDirection,
        anchor: Option<SessionListKey>,
        page_size: usize,
        limits: SessionListLimits,
    ) -> Result<Self, SessionListError> {
        let page_size =
            NonZeroUsize::new(page_size).ok_or(SessionListError::PageLimitExceeded {
                actual: 0,
                maximum: limits.page_rows.get(),
            })?;
        if page_size.get() > limits.page_rows.get() {
            return Err(SessionListError::PageLimitExceeded {
                actual: page_size.get(),
                maximum: limits.page_rows.get(),
            });
        }
        if let SessionListScope::Directory(location) = &scope {
            if let (Some(in_location), Some(explicit)) = (location.workspace(), workspace.as_ref())
            {
                if in_location != explicit {
                    return Err(SessionListError::InvalidCriteria {
                        reason: "directory scope and workspace filter name different workspaces"
                            .to_owned(),
                    });
                }
            }
        }
        Ok(Self {
            scope,
            workspace,
            title_contains,
            parent,
            order,
            direction,
            anchor,
            page_size,
            limits,
        })
    }

    pub(super) fn effective_workspace(&self) -> Option<&WorkspaceId> {
        self.workspace.as_ref().or_else(|| match &self.scope {
            SessionListScope::Directory(location) => location.workspace(),
            SessionListScope::All | SessionListScope::Project(_) => None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionListProvenance {
    LegacyBaseline { digest: String, cutover_seq: i64 },
    RustEvent { digest: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionListItem {
    pub key: SessionListKey,
    pub project_id: ProjectId,
    pub workspace_id: Option<WorkspaceId>,
    pub parent_id: Option<SessionId>,
    pub directory: String,
    pub title: String,
    pub provenance: SessionListProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionListPage {
    pub(super) items: Vec<SessionListItem>,
}

impl SessionListPage {
    pub fn items(&self) -> &[SessionListItem] {
        &self.items
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionListError {
    #[error("invalid session-list criteria: {reason}")]
    InvalidCriteria { reason: String },
    #[error("page size {actual} exceeds positive maximum {maximum}")]
    PageLimitExceeded { actual: usize, maximum: usize },
    #[error("search is {actual} UTF-8 bytes; maximum is {maximum}")]
    SearchLimitExceeded { actual: usize, maximum: usize },
    #[error("session-list query exceeded its SQLite VM-step budget")]
    QueryBudgetExceeded,
    #[error("session-list projection is unavailable")]
    ProjectionUnavailable,
    #[error("session-list projection is corrupt: {reason}")]
    ProjectionCorrupt { reason: String },
    #[error("session-list store operation failed: {source}")]
    StoreFailure {
        #[source]
        source: rusqlite::Error,
    },
}
