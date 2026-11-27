//! Exclude copied context only when the frozen parent proves its usage.
use super::*;

pub(super) fn read(
    source: &grapt_store::read_snapshot::ReadSnapshot<'_>,
    session: &SessionId,
    snapshot: &mut AccountingSnapshot,
) -> Result<MessageHistory, String> {
    let mut statement = source
        .prepare(crate::fork_boundary::SQL)
        .map_err(|e| e.to_string())?;
    let mut boundary_statement = source
        .prepare(crate::fork_boundary::BOUNDARY_SQL)
        .map_err(|e| e.to_string())?;
    let Some(boundary) =
        crate::fork_boundary::read(&mut statement, &mut boundary_statement, session)
            .map_err(|e| e.to_string())?
    else {
        return Ok(MessageHistory::default());
    };
    let mut parent = MessageHistory::default();
    if boundary.parent_seq() == grapt_types::ParentSeq::Nothing {
        return Ok(parent);
    }
    let mut statement = source
        .prepare(
            "SELECT seq, CASE WHEN length(CAST(type AS BLOB)) <= 128 THEN type END,
             CASE WHEN length(CAST(data AS BLOB)) <= ?3 THEN data END
         FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
           AND seq <= ?2 ORDER BY seq LIMIT ?4",
        )
        .map_err(|e| e.to_string())?;
    let mut rows = statement
        .query(rusqlite::params![
            boundary.parent().as_str(),
            boundary.parent_seq().as_raw(),
            ROW_BYTES as i64,
            (MAX_ACCOUNTING_ROWS + 1) as i64
        ])
        .map_err(|e| e.to_string())?;
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        if snapshot.events_read == MAX_ACCOUNTING_ROWS {
            return Err("accounting inherited row bound exceeded".into());
        }
        let seq: i64 = row.get(0).map_err(|e| e.to_string())?;
        let kind: Option<String> = row.get(1).map_err(|e| e.to_string())?;
        let data: Option<String> = row.get(2).map_err(|e| e.to_string())?;
        let kind = kind.ok_or("inherited accounting kind exceeds its bound")?;
        let data = data.ok_or("inherited accounting data exceeds its bound")?;
        snapshot.events_read += 1;
        snapshot.bytes_read = snapshot
            .bytes_read
            .checked_add(kind.len() + data.len())
            .ok_or("accounting input byte overflow")?;
        if snapshot.bytes_read > MAX_ACCOUNTING_BYTES {
            return Err("accounting input exceeds 64 MiB".into());
        }
        let Some(row) = super::super::store::classify_row(&kind, seq, &data)
            .map_err(|(kind, reason)| row_error(seq, &kind, reason))?
        else {
            continue;
        };
        parent
            .apply(HistoryRow {
                seq: row.seq,
                kind_name: row.name,
                kind_version: row.version,
                data: &row.value,
            })
            .map_err(|e| e.to_string())?;
        check_attempt_bound(&parent, 0)?;
        if parent.usage.values().any(|values| values.len() > 64) {
            return Err("inherited attempt exceeds 64 usage observations".into());
        }
    }
    Ok(parent)
}

pub(super) fn identities(
    parent: &MessageHistory,
    child: &MessageHistory,
) -> Result<HashSet<PhysicalAttemptId>, String> {
    let inherited: HashSet<_> = parent
        .attempt_messages
        .keys()
        .chain(parent.usage.keys())
        .cloned()
        .collect();
    for (attempt, observations) in &child.usage {
        if !inherited.contains(attempt) {
            continue;
        }
        let frozen = parent.usage_for(attempt);
        if observations.iter().any(|usage| !frozen.contains(usage)) {
            return Err("copied usage differs from frozen parent evidence".into());
        }
    }
    Ok(inherited)
}
