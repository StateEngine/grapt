//! Bounded, parent-scoped views of durable child operations.

use super::*;

const PAGE_SIZE: usize = 64;

/// A child address and lifecycle label, without its output or private transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildOperation {
    pub operation: SubagentOperationId,
    pub child: SessionId,
    pub mode: SubagentMode,
    pub status: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildOperationPage {
    pub children: Vec<ChildOperation>,
    /// Pass this operation as `after` to read the next page.
    pub next: Option<SubagentOperationId>,
}

impl SubagentService {
    /// List direct children in operation-id order. Missing tables mean no
    /// admissions; unreadable or corrupt rows remain errors. This never installs
    /// a schema or changes an operation, and retains at most 65 row addresses.
    /// Lifecycle states can advance while a page is read; parent routing is
    /// rechecked after strict decoding of each row.
    pub fn list_children_on(
        store: &EventStore,
        parent: &SessionId,
        after: Option<&SubagentOperationId>,
    ) -> Result<ChildOperationPage, SubagentError> {
        let conn = store.connection();
        let installed: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'subagent_operation')",
            [],
            |row| row.get(0),
        )?;
        let mut page = ChildOperationPage {
            children: Vec::new(),
            next: None,
        };
        if !installed {
            return Ok(page);
        }
        let sql = if followup::installed(conn)? {
            "WITH roots(operation_id) AS (
               SELECT o.operation_id FROM subagent_operation o
                WHERE o.parent_session_id=?1 AND NOT EXISTS(
                  SELECT 1 FROM subagent_run r WHERE r.operation_id=o.operation_id)
               UNION
               SELECT MIN(o.operation_id) FROM subagent_operation o
                WHERE o.parent_session_id=?1 GROUP BY o.child_session_id
                HAVING SUM(CASE WHEN NOT EXISTS(
                  SELECT 1 FROM subagent_run r WHERE r.operation_id=o.operation_id)
                  THEN 1 ELSE 0 END)=0
             ) SELECT CASE WHEN length(CAST(operation_id AS BLOB))<=512 THEN operation_id END
               FROM roots WHERE ?2 IS NULL OR operation_id>?2
               ORDER BY operation_id LIMIT 65"
        } else {
            "SELECT CASE WHEN length(CAST(operation_id AS BLOB))<=512 THEN operation_id END
             FROM subagent_operation WHERE parent_session_id=?1
               AND (?2 IS NULL OR operation_id>?2) ORDER BY operation_id LIMIT 65"
        };
        // A group with no root still yields a bounded address, so corrupt
        // successor cycles cannot disappear from the listing as an empty page.
        let mut query = conn.prepare(sql)?;
        let addresses = query
            .query_map(
                rusqlite::params![parent.as_str(), after.map(SubagentOperationId::as_str)],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let mut last_root = None;
        for raw in addresses.iter().take(PAGE_SIZE) {
            let operation = SubagentOperationId::new(raw.clone()).ok_or_else(|| {
                SubagentError::CorruptOperationId {
                    raw: raw.clone(),
                    reason: "invalid listed operation id".into(),
                }
            })?;
            let row = Self::decode_bounded_row(conn, &operation)?;
            if row.parent != *parent {
                return Err(SubagentError::Corruption {
                    operation,
                    reason: "listed operation changed parent".into(),
                });
            }
            let chain =
                followup::read_chain(conn, &row.child)?.ok_or_else(|| SubagentError::Followup {
                    reason: "listed child lost its operation".into(),
                })?;
            if chain.root != operation || chain.current.parent != *parent {
                return Err(SubagentError::Followup {
                    reason: "listed child changed its root or parent".into(),
                });
            }
            last_root = Some(operation);
            page.children.push(ChildOperation {
                operation: chain.current.operation,
                child: row.child,
                mode: chain.current.mode,
                status: status_label(&chain.current.status),
            });
        }
        if addresses.len() > PAGE_SIZE {
            page.next = last_root;
        }
        Ok(page)
    }
}
