//! The open requests, read from the store alone. Listing needs no approval
//! state, so a pinned read (a session snapshot's lease) lists them without
//! the writer-owned lock that guards approvals.

use super::*;

impl PermissionHttpState {
    /// The open requests, read from the store alone: no approval state, so a
    /// pinned read (a snapshot's lease) needs no writer-owned lock to list them.
    pub fn list_from(
        store: &EventStore,
        owner: &PermissionOwner<'_>,
        session: Option<&SessionId>,
    ) -> Result<PermissionHttpPage, PermissionHttpError> {
        let mut statement = store.connection().prepare(
            "SELECT p.id, p.session_id, p.action, p.status, p.suspended_reason,
                    h.agent_id, h.resources_json, h.save_json, h.metadata_json, h.source_json
             FROM permission_request p JOIN permission_http_request h ON h.id = p.id
             WHERE h.owner_subject = ?1 AND h.owner_client = ?2
               AND (?3 IS NULL OR p.session_id = ?3)
               AND p.status IN ('pending', 'suspended')
             ORDER BY p.rowid DESC LIMIT ?4",
        )?;
        let rows = statement.query_map(
            rusqlite::params![
                owner.subject,
                owner.client.as_str(),
                session.map(SessionId::as_str),
                i64::try_from(MAX_HTTP_PERMISSION_ROWS + 1).map_err(|_| {
                    PermissionHttpError::ApprovalBound {
                        max: MAX_HTTP_PERMISSION_ROWS,
                    }
                })?
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            },
        )?;
        let mut requests = rows
            .map(|row| decode_row(row.map_err(PermissionError::Sqlite)?))
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = requests.len() > MAX_HTTP_PERMISSION_ROWS;
        requests.truncate(MAX_HTTP_PERMISSION_ROWS);
        Ok(PermissionHttpPage { requests, has_more })
    }
}
