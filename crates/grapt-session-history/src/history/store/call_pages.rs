//! Shared exact-call history selection and its single strict row decoder.
use super::*;

type Row = (String, i64, i64, Option<String>);
const SHARED_DATA_BYTES: usize = 512 * 1024;

// Keep the ownership boundary observable in unit tests without an allocator or
// RSS-dependent assertion. Every raw probe data copy goes through this helper.
fn materialize_data(data: &str) -> String {
    #[cfg(test)]
    COPIED_DATA_BYTES.with(|bytes| bytes.set(bytes.get().saturating_add(data.len())));
    data.to_owned()
}

#[cfg(test)]
std::thread_local! {
    static COPIED_DATA_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests;

pub(super) fn sql(
    select_data: &str,
    target_predicate: &str,
    extra_columns: &str,
    visible: &str,
) -> String {
    format!(
        "SELECT id, seq, created, {select_data}{extra_columns} FROM event
                 WHERE aggregate_family = 'session' AND aggregate_id = ?1 AND {visible} AND (seq >= ?3 OR seq < 0)
                   AND (?5 IS NULL OR seq < ?5)
                   AND type = 'session.content.appended.1'
                   AND CASE
                         WHEN json_valid(data) = 0 THEN 1
                         WHEN json_type(data, '$.item.kind') IS NULL THEN 1
                         WHEN json_type(data, '$.item.kind') != 'text' THEN 1
                         WHEN json_extract(data, '$.item.kind') IN ('text', 'reasoning', 'opaque')
                           THEN 0
                           ELSE 1
                       END = 1
                   AND CASE WHEN ?4 IS NULL OR json_valid(data) = 0 THEN 1
                         WHEN json_extract(data, '$.item.kind') IS NOT 'tool' THEN 1
                         WHEN json_type(data, '$.item.call') IS NOT 'text' THEN 1
                         ELSE json_extract(data, '$.item.call') {target_predicate} END
                 ORDER BY seq LIMIT ?2"
    )
}

pub(super) fn assemble(
    session: &SessionId,
    rows: Vec<Row>,
    max_rows: usize,
    max_data_bytes: Option<usize>,
) -> Result<SessionToolCallPage, HistoryReadError> {
    if rows.len() > max_rows {
        return Err(HistoryReadError::ToolCallPageTooLong {
            session: session.as_str().to_owned(),
            limit: max_rows,
        });
    }
    let mut window_bytes = 0usize;
    let mut calls = Vec::with_capacity(rows.len());
    for (event, seq, created_ms, data) in rows {
        let corrupt = |reason: String| HistoryReadError::Corrupt {
            session: session.as_str().to_owned(),
            seq,
            kind: "session.content.appended".to_owned(),
            reason,
        };
        // A stored sequence below zero cannot locate a row, so it refuses
        // BEFORE any payload or capacity decision, as the range scan and the
        // fold do. Deciding capacity first would report a corrupt coordinate as
        // an oversized row and name the wrong defect.
        if seq < 0 {
            return Err(corrupt(format!("the stored sequence is {seq}")));
        }
        let data = match data {
            Some(data) => data,
            None => {
                let limit = max_data_bytes.unwrap_or(usize::MAX);
                return Err(corrupt(format!(
                    "the stored data exceeds the {limit}-byte window bound"
                )));
            }
        };
        if let Some(limit) = max_data_bytes {
            window_bytes = window_bytes.saturating_add(data.len());
            if window_bytes > limit {
                return Err(HistoryReadError::WindowDataTooLong {
                    session: session.as_str().to_owned(),
                    limit,
                });
            }
        }
        let value = serde_json::from_str::<Value>(&data)
            .map_err(|_| corrupt("the stored data is not JSON".to_owned()))?;
        let fact =
            session_message::decode_appended(&value).map_err(|error| corrupt(error.to_string()))?;
        let ContentKind::Tool { call } = fact.item().kind() else {
            return Err(corrupt(
                "the tool-call page selected non-tool content".to_owned(),
            ));
        };
        let event = event
            .parse::<EventId>()
            .map_err(|_| corrupt(format!("the stored event identity {event:?} is invalid")))?;
        let seq =
            u64::try_from(seq).map_err(|_| corrupt(format!("the stored sequence is {seq}")))?;
        calls.push(DurableToolCall {
            message: fact.message().clone(),
            call: call.clone(),
            payload: fact.item().payload().clone(),
            disposition: fact.disposition(),
            committed: CommittedPosition {
                event,
                seq,
                created_ms,
            },
        });
    }
    Ok(SessionToolCallPage { rows: calls })
}

pub(super) fn read(
    conn: &rusqlite::Connection,
    session: &SessionId,
    calls: &[ToolCallId],
) -> Result<Option<ToolCallPages>, HistoryReadError> {
    let corrupt = |reason: &str| HistoryReadError::Corrupt {
        session: session.as_str().to_owned(),
        seq: 0,
        kind: "selected tool calls".to_owned(),
        reason: reason.to_owned(),
    };
    if calls.is_empty() || calls.len() > SessionHistoryStore::CALL_BATCH_SIZE {
        return Err(corrupt("invalid selected-call batch bound"));
    }
    let store = |source| HistoryReadError::Store {
        session: session.as_str().to_owned(),
        source,
    };
    let selected = serde_json::to_string(&calls.iter().map(ToolCallId::as_str).collect::<Vec<_>>())
        .map_err(|_| corrupt("selected call identities could not be encoded"))?;
    let query = sql(
        "data",
        "IN (SELECT value FROM json_each(?4))",
        ", CASE WHEN json_valid(data) = 0 THEN NULL
          WHEN json_extract(data, '$.item.kind') IS NOT 'tool' THEN NULL
          WHEN json_type(data, '$.item.call') = 'text' THEN json_extract(data, '$.item.call')
          ELSE NULL END",
        &visibility(conn, session)?.sql("seq"),
    );
    let mut statement = conn.prepare(&query).map_err(store)?;
    let limit = i64::try_from(MAX_TOOL_CALL_PAGE_ROWS.saturating_add(1))
        .map_err(|_| corrupt("selected call row bound exceeds SQLite range"))?;
    let mut cursor = statement
        .query((
            session.as_str(),
            limit,
            i64::MIN,
            selected,
            Option::<i64>::None,
        ))
        .map_err(store)?;
    let mut rows = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = cursor.next().map_err(store)? {
        if rows.len() == MAX_TOOL_CALL_PAGE_ROWS {
            return Ok(None);
        }
        let value = row.get_ref(3).map_err(store)?;
        let data = match value.as_str_or_null() {
            Ok(data) => data,
            Err(_) => return Ok(None),
        };
        let next_bytes = bytes.saturating_add(data.map_or(0, str::len));
        // Inspect SQLite's borrowed value before creating an owned data copy.
        if next_bytes > SHARED_DATA_BYTES {
            return Ok(None);
        }
        let header: rusqlite::Result<_> = (|| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })();
        let (event, seq, created, key) = match header {
            Ok(header) => header,
            Err(error) if error.sqlite_error().is_none() => return Ok(None),
            Err(error) => return Err(store(error)),
        };
        rows.push(((event, seq, created, data.map(materialize_data)), key));
        bytes = next_bytes;
    }
    Ok(Some(
        calls
            .iter()
            .map(|call| {
                let selected = rows
                    .iter()
                    .filter(|(_, key)| key.as_deref().is_none_or(|key| key == call.as_str()))
                    .map(|(row, _)| row.clone())
                    .collect();
                assemble(session, selected, MAX_TOOL_CALL_PAGE_ROWS, None)
            })
            .collect(),
    ))
}
