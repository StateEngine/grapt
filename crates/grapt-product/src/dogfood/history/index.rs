// Copyright 2025 OpenAI
// SPDX-License-Identifier: Apache-2.0
// Keyset direction and SQL limit.
// Modified by Grapt: durable sequence, visible public sources, existing decoders.
//! Select public identities before reading payloads or rendering a history page.
use super::*;
use grapt_store::commit_projection::ProjectionConnection as Connection;
use std::collections::BTreeMap;

struct PublicRow {
    seq: u64,
    id: String,
    source: IndexSource,
}

#[derive(Debug, Clone, Copy)]
enum IndexSource {
    Input,
    Assistant,
    Compaction,
    Failure,
    Notice,
}

fn sources(connection: &Connection, session: &SessionId) -> Result<String, String> {
    let visible = grapt_store::history_visibility::HistoryVisibility::read(connection, session)
        .map_err(|error| error.to_string())?
        .sql("seq");
    let notices = grapt_session::notice::public_notice_predicate("data");
    Ok(format!("WITH public AS (
        SELECT seq, CASE WHEN length(CAST(id AS BLOB))<=512 THEN id END AS id, 0 AS source
        FROM session_message WHERE session_id=?1 AND type IN ('user','synthetic') AND {visible}
        UNION ALL
        SELECT seq, CASE WHEN (type='session.assistant-message.opened' OR type GLOB 'session.assistant-message.opened.*') THEN
            CASE WHEN length(CAST(data AS BLOB))<=16384 AND json_valid(data) AND length(CAST(json_extract(data,'$.message') AS BLOB))<=512
                 THEN json_extract(data,'$.message') END
            ELSE CASE WHEN length(CAST(id AS BLOB))<=256 AND substr(id,1,4)='evt_'
                 THEN 'msg_'||substr(id,5) END END,
            CASE WHEN (type='session.assistant-message.opened' OR type GLOB 'session.assistant-message.opened.*') THEN 1
                 WHEN type='session.compaction.ended.2' THEN 2
                 WHEN type='session.execution.failed.1' THEN 3 ELSE 4 END
        FROM event WHERE aggregate_family='session' AND aggregate_id=?1 AND {visible}
        AND ((type='session.assistant-message.opened' OR type GLOB 'session.assistant-message.opened.*') OR type='session.compaction.ended.2'
            OR type='session.execution.failed.1' OR (type='session.notice.recorded.1' AND ({notices}))))"))
}

fn selected(raw: (i64, Option<String>, i64)) -> Result<PublicRow, String> {
    let (seq, id, source) = raw;
    let seq = u64::try_from(seq).map_err(|_| "public history sequence is negative")?;
    let source = match source {
        0 => IndexSource::Input,
        1 => IndexSource::Assistant,
        2 => IndexSource::Compaction,
        3 => IndexSource::Failure,
        4 => IndexSource::Notice,
        _ => return Err("public history source is invalid".into()),
    };
    let Some(id) = id else {
        // An assistant opening whose own payload did not decode cannot name the
        // message it opened, so it has no public identity to select. It is not
        // silently lost: the page's span scan reports it as a named gap, and the
        // rows around it still render. Only a row that SHOULD have an identity
        // and has none is a contradiction.
        if matches!(source, IndexSource::Assistant) {
            return Err(UNREADABLE_OPENING.into());
        }
        return Err(format!(
            "public history row at {seq} has no bounded identity"
        ));
    };
    if WireId::<MessageId>::parse(id.clone()).is_none() {
        return Err(format!(
            "public history row at {seq} has an invalid identity"
        ));
    }
    Ok(PublicRow { seq, id, source })
}

/// Marker returned by [`selected`] for an opening that cannot name itself.
///
/// It is recognised at the selection boundary and skipped, because the gap is
/// already reported by the page's span scan and refusing the page would hide
/// every other message on it.
const UNREADABLE_OPENING: &str = "public history opening is unreadable";

/// Rows beyond the requested page one read may skip while refilling it.
///
/// An unreadable opening has no identity to return, so the page keeps selecting
/// until it has a full page or runs out of history. Without a bound, a Session
/// whose openings are all unreadable would walk its whole history on one poll.
/// Past this many skipped rows the read refuses and names the bound, because an
/// empty page with no cursor would read as "no further history".
const MAX_PAGE_REFILL_ROWS: usize = 512;

fn unique_sequence(
    connection: &Connection,
    sql: &str,
    session: &SessionId,
    sequence: u64,
) -> Result<(), String> {
    let sequence = i64::try_from(sequence).map_err(|error| error.to_string())?;
    let mut query = connection
        .prepare(&format!(
            "{sql} SELECT seq FROM public WHERE seq=?2 LIMIT 2"
        ))
        .map_err(|error| error.to_string())?;
    let count = query
        .query_map((session.as_str(), sequence), |row| row.get::<_, i64>(0))
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?
        .len();
    if count > 1 {
        return Err("two public messages claim the same durable sequence".into());
    }
    Ok(())
}

fn lookup(
    connection: &Connection,
    sql: &str,
    session: &SessionId,
    id: &str,
) -> Result<Option<PublicRow>, String> {
    let mut query = connection
        .prepare(&format!(
            "{sql} SELECT seq,id,source FROM public WHERE id=?2 LIMIT 2"
        ))
        .map_err(|error| error.to_string())?;
    let mut rows = query
        .query_map((session.as_str(), id), |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    if rows.len() > 1 {
        return Err("two public messages claim the same identity".into());
    }
    let row = rows.pop().map(selected).transpose()?;
    if let Some(row) = &row {
        unique_sequence(connection, sql, session, row.seq)?;
    }
    Ok(row)
}

/// Select one page's public rows, skipping openings that cannot name
/// themselves.
///
/// Returns whether the walk is ascending, the rows the page will return (at
/// most `limit`), and EVERY sequence the walk visited, which is what bounds the
/// gap span afterwards.
///
/// An opening whose own payload did not decode cannot name itself, so it has no
/// public identity and cannot be a row; it is skipped and the read moves on.
/// Truncating to `limit` before that skip lets one unreadable opening consume
/// the whole page and return `data: []` with no cursor, which reads as "no
/// further history" — the exact lie this page must not tell.
///
/// The refill is bounded: past [`MAX_PAGE_REFILL_ROWS`] skipped rows in one
/// direction nothing displayable remains to carry a cursor, so the read refuses
/// and names the bound rather than serving an empty page.
fn select_page_rows(
    connection: &Connection,
    sql: &str,
    session: &SessionId,
    page: &MessagePage,
    anchor: Option<i64>,
) -> Result<(bool, Vec<PublicRow>, Vec<i64>), PageRefusal> {
    let ascending = page.ascending();
    let operator = if ascending { ">" } else { "<" };
    let order = if ascending { "ASC" } else { "DESC" };
    let mut query = connection
        .prepare(&format!(
            "{sql} SELECT seq,id,source FROM public WHERE (?2 IS NULL OR seq {operator} ?2)
             ORDER BY seq {order} LIMIT ?3"
        ))
        .map_err(|error| error.to_string())?;
    let probe = i64::try_from(page.limit + 1).map_err(|error| error.to_string())?;
    let refill = page.limit.saturating_add(MAX_PAGE_REFILL_ROWS);
    let mut cursor = anchor;
    let mut visited: Vec<i64> = Vec::new();
    let mut rows: Vec<PublicRow> = Vec::new();
    while rows.len() <= page.limit {
        if visited.len() > refill {
            return Err(format!(
                "more than {refill} public history rows in this direction are unreadable; \
                 the page cannot name a cursor past them"
            )
            .into());
        }
        let batch = query
            .query_map((session.as_str(), cursor, probe), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let Some(last) = batch.last().map(|row| row.0) else {
            break;
        };
        if batch.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err("two public messages claim the same durable sequence"
                .to_owned()
                .into());
        }
        for raw in batch {
            visited.push(raw.0);
            match selected(raw) {
                Ok(row) => rows.push(row),
                Err(error) if error == UNREADABLE_OPENING => {}
                Err(error) => return Err(error.into()),
            }
        }
        cursor = Some(last);
    }
    rows.truncate(page.limit);
    Ok((ascending, rows, visited))
}

impl DogfoodApplication {
    /// The rows one page carries, and the unreadable owned rows inside the
    /// same span. The span is the page's own cursor range, not the returned
    /// rows': an unreadable row usually has no rendered peer, so a span taken
    /// from the rows would miss exactly the case this reports.
    pub(crate) fn public_page(
        &self,
        session: &SessionId,
        page: &MessagePage,
    ) -> Result<(Vec<IndexedRow>, Vec<grapt_session::HistoryOmission>), PageRefusal> {
        let selected = {
            let held = self.lock_inbox();
            let connection = held.store().connection();
            let sql = sources(connection, session)?;
            let anchor = page
                .cursor
                .as_ref()
                .map(|cursor| {
                    lookup(connection, &sql, session, cursor.id.as_inner().as_str())?
                        .ok_or(PageRefusal::StaleCursor)
                })
                .transpose()?;
            let anchor = anchor
                .map(|row| i64::try_from(row.seq))
                .transpose()
                .map_err(|error| error.to_string())?;
            let (ascending, selected_rows, visited) =
                select_page_rows(connection, &sql, session, page, anchor)?;
            // A page's span is the durable range its own rows cover, extended
            // to the next public row this page did not return. Content rows
            // FOLLOW their message's opening row, so a span taken from the
            // rendered rows alone would stop short of exactly the rows that can
            // fail to decode. The bounds therefore come from the EFFECTIVE SQL
            // direction, not from `previous()`: an ascending walk covers
            // `(anchor, boundary)` and a descending walk covers
            // `(boundary, anchor)`. Using the cursor direction instead scans the
            // wrong range — an initial descending page would scan from the
            // beginning of history, and a descending next page would produce an
            // empty span because its anchor sits above its probe.
            let last_returned = selected_rows
                .last()
                .map(|row| i64::try_from(row.seq))
                .transpose()
                .map_err(|error| error.to_string())?;
            // The boundary is the NEAREST visited row beyond the last returned
            // one. An unreadable opening skipped inside the page is therefore
            // scanned here (its content falls between two returned rows), and
            // one skipped past the page's end is left to the next page.
            let boundary = if ascending {
                visited
                    .iter()
                    .copied()
                    .filter(|seq| last_returned.is_none_or(|last| *seq > last))
                    .min()
            } else {
                visited
                    .iter()
                    .copied()
                    .filter(|seq| last_returned.is_none_or(|last| *seq < last))
                    .max()
            };
            let (after, through) = if ascending {
                // A page that ran out of history has no next row, so its span
                // reaches the end: the older tail it returned is exactly what
                // must be checked for gaps.
                (
                    anchor,
                    boundary
                        .map(|seq| seq.saturating_sub(1))
                        .unwrap_or(i64::MAX),
                )
            } else {
                (
                    boundary,
                    anchor.map(|seq| seq.saturating_sub(1)).unwrap_or(i64::MAX),
                )
            };
            // An empty or unrepresentable span reports no gaps, never a
            // made-up one: a negative coordinate cannot bound a real range.
            // `None` for `after` is the honest lower bound of the Session's
            // history, which is what an initial ascending page starts from.
            let after = after.and_then(|after| u64::try_from(after).ok());
            let through = u64::try_from(through).ok();
            let mut rows = selected_rows;
            if page.previous() {
                rows.reverse();
            }
            // A failed scan is not "no gap": it would present a page as
            // complete without having checked. The page's own rows still
            // refuse the read through the same error, as the skip-notice scan
            // above does.
            let omissions = match through {
                Some(through) if after.is_none_or(|after| after < through) => {
                    grapt_session::SessionHistoryStore::over(held.store())
                        .omissions_between(session, after, through)
                        .map_err(|error| error.to_string())?
                }
                _ => Vec::new(),
            };
            (rows, omissions)
        };
        let (rows, omissions) = selected;
        let coordinates = self.input_coordinates(&rows)?;
        let rows = rows
            .into_iter()
            .map(|row| {
                self.public_row(session, row, &coordinates)
                    .map_err(PageRefusal::Failed)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((rows, omissions))
    }

    pub(crate) fn public_message(
        &self,
        session: &SessionId,
        id: &MessageId,
    ) -> Result<Option<IndexedRow>, String> {
        let row = {
            let held = self.lock_inbox();
            let connection = held.store().connection();
            lookup(
                connection,
                &sources(connection, session)?,
                session,
                id.as_str(),
            )?
        };
        row.map(|row| {
            let coordinates = self.input_coordinates(std::slice::from_ref(&row))?;
            self.public_row(session, row, &coordinates)
        })
        .transpose()
    }

    fn input_coordinates(
        &self,
        rows: &[PublicRow],
    ) -> Result<BTreeMap<MessageId, grapt_inbox::AdmissionCoordinates>, String> {
        let inputs = rows
            .iter()
            .filter(|row| matches!(row.source, IndexSource::Input))
            .map(|row| {
                row.id
                    .parse::<MessageId>()
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let coordinates = self
            .lock_inbox()
            .admission_coordinate_page(&inputs)
            .map_err(|error| error.to_string())?;
        Ok(inputs.into_iter().zip(coordinates).collect())
    }

    fn public_row(
        &self,
        session: &SessionId,
        row: PublicRow,
        coordinates: &BTreeMap<MessageId, grapt_inbox::AdmissionCoordinates>,
    ) -> Result<IndexedRow, String> {
        let loaded = match row.source {
            IndexSource::Input => self.promoted_user_row_at(
                session,
                row.seq,
                *coordinates
                    .get(
                        &row.id
                            .parse::<MessageId>()
                            .map_err(|error| error.to_string())?,
                    )
                    .ok_or("selected input has no admission coordinates")?,
            )?,
            IndexSource::Assistant => {
                let held = self.lock_inbox();
                let selected = grapt_session::SessionHistoryStore::over(held.store())
                    .message_at(session, grapt_types::EventSeq::new(row.seq));
                match selected {
                    // The opening's own payload did not decode. Its identity
                    // came from the index, so the row is still real and the page
                    // still has something to show: a gap row keeps the position
                    // and says the message could not be read, instead of
                    // refusing every other message on the page.
                    Ok(None) => IndexedRow {
                        seq: row.seq,
                        id: row.id.clone(),
                        source: RowSource::Rendered(unreadable_message_row(&row.id, row.seq)?),
                    },
                    Ok(Some(message)) => IndexedRow {
                        seq: row.seq,
                        id: message.message.as_str().into(),
                        source: RowSource::Assistant(Box::new(message)),
                    },
                    // Any other refusal is a real read failure (a capacity
                    // bound included): naming it unreadable would hide a limit
                    // behind a data claim, so it still refuses.
                    Err(error) => return Err(format!("history row {}: {error}", row.id)),
                }
            }
            IndexSource::Compaction => {
                let committed = self
                    .compaction_at(session, row.seq)?
                    .ok_or("selected compaction is absent")?;
                IndexedRow {
                    seq: committed.seq,
                    id: committed.id.clone(),
                    source: RowSource::Compaction(committed),
                }
            }
            IndexSource::Failure => {
                let held = self.lock_inbox();
                let failure = grapt_session::execution::failure_at(
                    held.store().connection(),
                    session,
                    grapt_types::EventSeq::new(row.seq),
                )?
                .ok_or("selected failure is absent")?;
                let raw = failure_message(
                    &row.id,
                    failure.position.created_ms,
                    failure
                        .failure
                        .as_ref()
                        .ok_or("execution failure has no public class")?,
                )?;
                IndexedRow::rendered(failure.position.seq, row.id.clone(), raw)
            }
            IndexSource::Notice => {
                let held = self.lock_inbox();
                let notices = grapt_session::notice::read_notice_at(
                    held.store().connection(),
                    session,
                    grapt_types::EventSeq::new(row.seq),
                )
                .map_err(|error| format!("history row {}: {error}", row.id))?;
                let mut rows = Vec::new();
                super::super::history_notices::append(session, notices, &mut rows)
                    .map_err(|error| format!("history row {}: {error}", row.id))?;
                if rows.len() != 1 {
                    return Err(format!(
                        "history row {}: selected notice did not produce exactly one public row",
                        row.id
                    ));
                }
                rows.pop().ok_or("selected notice is absent")?
            }
        };
        if loaded.seq != row.seq || loaded.id != row.id {
            return Err("public history identity changed during its read".into());
        }
        Ok(loaded)
    }
}

/// A bounded identity probe into visible durable history, without reading payloads.
pub(crate) fn recovery_point(
    inbox: &grapt_inbox::Inbox,
    session: &SessionId,
    before: grapt_types::EventSeq,
) -> Result<(Option<MessageId>, Option<u64>), String> {
    let connection = inbox.store().connection();
    let sql = sources(connection, session)?;
    let stored = i64::try_from(before.value()).map_err(|error| error.to_string())?;
    let mut query = connection
        .prepare(&format!(
            "{sql} SELECT seq,id,source FROM public WHERE seq<?2 ORDER BY seq DESC LIMIT 1"
        ))
        .map_err(|error| error.to_string())?;
    let row = query
        .query_map((session.as_str(), stored), |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?
        .pop()
        .map(selected)
        .transpose()?;
    let message = row
        .map(|row| {
            unique_sequence(connection, &sql, session, row.seq)?;
            row.id
                .parse::<MessageId>()
                .map_err(|error| error.to_string())
        })
        .transpose()?;
    let event = grapt_store::session_event_queries::bounds::visible_head_before(
        inbox.store(),
        session,
        before,
    )?
    .map(grapt_types::EventSeq::value);
    Ok((message, event))
}
