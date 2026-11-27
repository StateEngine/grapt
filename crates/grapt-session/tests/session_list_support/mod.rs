//! Session-list fixtures shared by the list test binaries.
use grapt_session::{
    BoundedSearch, ParentFilter, SessionListBoundary, SessionListCriteria, SessionListDirection,
    SessionListError, SessionListLimits, SessionListOrder, SessionListScope,
};
use grapt_types::WorkspaceId;
use rusqlite::Connection;

#[allow(clippy::too_many_arguments)]
pub fn insert(
    conn: &Connection,
    id: &str,
    updated_ms: i64,
    project_id: &str,
    workspace_id: Option<&str>,
    parent_id: Option<&str>,
    directory: &str,
    title: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO session_list_legacy_baseline
            (session_id, updated_ms, source_domain, source_digest, cutover_seq)
         VALUES (?1, ?2, 'synthetic-fixture', 'digest', -1)",
        rusqlite::params![id, updated_ms],
    )?;
    conn.execute(
        "INSERT INTO session_list_projection
            (session_id, project_id, workspace_id, parent_id, directory, title, updated_ms,
             provenance_kind, provenance_digest, cutover_seq)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'legacy-baseline', 'digest', -1)",
        rusqlite::params![
            id,
            project_id,
            workspace_id,
            parent_id,
            directory,
            title,
            updated_ms
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn criteria(
    scope: SessionListScope,
    workspace: Option<WorkspaceId>,
    search: Option<&str>,
    parent: ParentFilter,
    order: SessionListOrder,
    direction: SessionListDirection,
    anchor: Option<SessionListBoundary>,
    page_size: usize,
    limits: SessionListLimits,
) -> Result<SessionListCriteria, SessionListError> {
    let title_contains = search
        .map(|value| BoundedSearch::new(value, limits))
        .transpose()?;
    SessionListCriteria::new(
        scope,
        workspace,
        title_contains,
        parent,
        order,
        direction,
        anchor,
        page_size,
        limits,
    )
}
