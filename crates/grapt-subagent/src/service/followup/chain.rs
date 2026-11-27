//! Validate every successor link instead of selecting a latest operation row.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn read(
    conn: &rusqlite::Connection,
    child: &SessionId,
) -> Result<Option<Chain>, SubagentError> {
    if child.as_str().len() > 512 {
        return Err(refused("child address exceeds its read bound"));
    }
    if installed(conn)? {
        let orphaned: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM subagent_run r WHERE r.child_session_id=?1
               AND NOT EXISTS(SELECT 1 FROM subagent_operation o
                 WHERE o.operation_id=r.operation_id AND o.child_session_id=r.child_session_id))
             OR EXISTS(SELECT 1 FROM subagent_followup f
               WHERE f.child_session_id=?1 AND f.state='started'
               AND NOT EXISTS(SELECT 1 FROM subagent_run r
                 WHERE r.operation_id=f.operation_id AND r.cause_message_id=f.message_id))",
            [child.as_str()],
            |row| row.get(0),
        )?;
        if orphaned {
            return Err(refused("child has an orphaned run or followup binding"));
        }
    }
    let mut query = conn.prepare(
        "SELECT CASE WHEN length(CAST(operation_id AS BLOB))<=512 THEN operation_id END
         FROM subagent_operation WHERE child_session_id=?1 ORDER BY operation_id LIMIT ?2",
    )?;
    let mut cursor = query.query(rusqlite::params![
        child.as_str(),
        i64::try_from(MAX_RUNS + 1).map_err(|_| refused("run bound overflow"))?
    ])?;
    let mut rows = BTreeMap::new();
    while let Some(row) = cursor.next()? {
        if rows.len() == MAX_RUNS {
            return Err(refused(
                "child exceeds 1024 retained runs; start a new agent",
            ));
        }
        let raw: Option<String> = row.get(0)?;
        let raw = raw.ok_or_else(|| refused("operation identity exceeds its read bound"))?;
        let operation = SubagentOperationId::new(raw.clone()).ok_or_else(|| {
            SubagentError::CorruptOperationId {
                raw,
                reason: "invalid child operation identity".into(),
            }
        })?;
        let stored = SubagentService::decode_bounded_row(conn, &operation)?;
        if stored.child != *child {
            return Err(refused("operation changed its child binding"));
        }
        rows.insert(operation, stored);
    }
    if rows.is_empty() {
        return Ok(None);
    }
    let mut roots = Vec::new();
    let mut next = BTreeMap::new();
    for operation in rows.keys() {
        match intent::run(conn, operation)? {
            Some(binding) => {
                if !rows.contains_key(&binding.predecessor)
                    || next
                        .insert(binding.predecessor, operation.clone())
                        .is_some()
                {
                    return Err(refused(
                        "run chain has an absent predecessor or multiple successors",
                    ));
                }
            }
            None => roots.push(operation.clone()),
        }
    }
    let [root] = roots.as_slice() else {
        return Err(SubagentError::AmbiguousChild {
            child: child.as_str().to_owned(),
            count: rows.len(),
        });
    };
    let parent = rows
        .get(root)
        .ok_or_else(|| refused("run chain lost its root"))?
        .parent
        .clone();
    if rows.values().any(|row| row.parent != parent) {
        return Err(refused(
            "successor changed the child's immutable parent route",
        ));
    }
    let mut current = root.clone();
    let mut seen = BTreeSet::new();
    loop {
        if !seen.insert(current.clone()) {
            return Err(refused("run chain contains a cycle"));
        }
        match next.get(&current) {
            Some(successor) => current = successor.clone(),
            None => break,
        }
    }
    if seen.len() != rows.len() {
        return Err(refused("run chain has an unreachable operation"));
    }
    let row = rows
        .remove(&current)
        .ok_or_else(|| refused("run chain lost its current operation"))?;
    Ok(Some(Chain {
        root: root.clone(),
        current: ChildOperationContext {
            operation: current,
            parent,
            mode: row.mode,
            status: row.status,
        },
    }))
}
