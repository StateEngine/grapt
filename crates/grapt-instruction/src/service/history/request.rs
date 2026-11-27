//! Bounded request projection of the existing instruction chronology.
//! The journal fold and cache validation remain owned by this module; writers
//! continue using their existing reconstruction path.
use super::*;
const MAX_ROWS: usize = 65_536;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_BLOB: i64 = 1024 * 1024;
fn bound() -> InstructionError {
    InstructionError::EntryBound {
        bound: "instruction request history",
    }
}
pub(in crate::service) fn blob_size(
    conn: &Connection,
    hash: InstructionHash,
) -> Result<usize, InstructionError> {
    let size: Option<i64> = conn
        .query_row(
            "SELECT length(CAST(value AS BLOB)) FROM instruction_blob WHERE hash = ?1",
            [hash.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    let size = size.ok_or_else(|| InstructionError::Corruption {
        reason: format!("instruction hash {hash} has no blob"),
    })?;
    if !(0..=MAX_BLOB).contains(&size) {
        return Err(bound());
    }
    usize::try_from(size).map_err(|_| bound())
}
/// Undecodable rows are skipped and named, the same rows [`fold_history`]
/// skips, so a request sees the fold every other reader sees.
pub(in crate::service) fn fold_request_history(
    conn: &Connection,
    session: &SessionId,
) -> Result<Fold, InstructionError> {
    fold(conn, session, FoldMode::Current)
}

pub(super) fn legacy_fork_history(
    conn: &Connection,
    session: &SessionId,
) -> Result<Option<FoldedHistory>, InstructionError> {
    // Compatibility recognition shares the bounded decoder and event fold;
    // it is never an alternative source of current instruction truth. A
    // degraded fold recognizes nothing.
    let fold = fold(conn, session, FoldMode::LegacyFork)?;
    Ok(if fold.skipped.is_empty() {
        fold.history
    } else {
        None
    })
}

fn fold(conn: &Connection, session: &SessionId, mode: FoldMode) -> Result<Fold, InstructionError> {
    let mut folded = None;
    let mut skipped = Vec::new();
    let mut count = 0usize;
    let mut bytes = 0usize;
    let lineage = own_lineage(conn, session)?;
    for (index, segment) in lineage.segments().iter().enumerate() {
        let mut statement = conn.prepare(
            "SELECT id, seq, created, type, data, length(CAST(data AS BLOB)), length(CAST(id AS BLOB)), length(CAST(type AS BLOB)) FROM event
             WHERE aggregate_id = ?1 AND (?2 IS NULL OR seq <= ?2)
             AND (type LIKE 'session.instructions.updated.%' OR type LIKE 'session.compaction.ended.%'
               OR type LIKE 'session.moved.%' OR type LIKE 'session.revert.committed.%')
             ORDER BY seq LIMIT 65537",
        )?;
        let cap = match mode {
            FoldMode::Current => segment.cap().bound(),
            FoldMode::LegacyFork => lineage.segments()[index..]
                .iter()
                .filter_map(|segment| segment.cap().bound())
                .min(),
        }
        .map(grapt_types::ParentSeq::as_raw);
        let mut rows = statement.query(rusqlite::params![segment.session().as_str(), cap])?;
        while let Some(row) = rows.next()? {
            count += 1;
            let size: i64 = row.get(5)?;
            if count > MAX_ROWS
                || !(0..=65_536).contains(&size)
                || !(1..=256).contains(&row.get::<_, i64>(6)?)
                || !(1..=128).contains(&row.get::<_, i64>(7)?)
            {
                return Err(bound());
            }
            bytes = bytes
                .checked_add(usize::try_from(size).map_err(|_| bound())?)
                .ok_or_else(bound)?;
            if bytes > MAX_BYTES {
                return Err(bound());
            }
            let (event, payload) = match decode_journal_row(row, segment.session()) {
                Ok(decoded) => decoded,
                Err(error) if row_damage(&error) => {
                    skipped.push(skip(row, segment.session(), &error));
                    continue;
                }
                Err(error) => return Err(error),
            };
            if let Payload::Updated(delta) = &payload {
                if delta.len() > 256 || delta.keys().any(|key| key.as_str().len() > 256) {
                    return Err(bound());
                }
                // Each blob is charged to the read's budget from its stored
                // length before it is opened, and stays charged if the row
                // turns out damaged: skipping never buys more reading.
                let mut charge = |hash: &InstructionHash| -> Result<(), InstructionError> {
                    bytes = bytes
                        .checked_add(blob_size(conn, *hash)?)
                        .ok_or_else(bound)?;
                    if bytes > MAX_BYTES {
                        return Err(bound());
                    }
                    Ok(())
                };
                let checked = delta
                    .values()
                    .flatten()
                    .try_for_each(&mut charge)
                    .and_then(|()| row_blobs(conn, &payload));
                match checked {
                    Ok(()) => {}
                    Err(error) if row_damage(&error) => {
                        skipped.push(skip(row, segment.session(), &error));
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
            apply_payload(&mut folded, &event, payload, mode)?;
            if folded.as_ref().is_some_and(|history| {
                history.state.initial.len() > 256 || history.state.current.len() > 256
            }) {
                return Err(bound());
            }
        }
    }
    Ok(Fold {
        history: folded,
        skipped,
    })
}
