//! Select a complete recent suffix without fetching any more artifacts.
use super::*;

/// A small exact safety net beside the summary and original user messages.
pub(super) const TOKEN_BUDGET: u64 = 8_000;

/// Newest-first bounded retention (Apache-2.0).
/// SE retains whole exchanges so calls keep their outputs.
pub(super) fn cut(
    (inbox, session): (&Inbox, &SessionId),
    context: &RequestContext,
    (current, head): (u64, u64),
    candidates: impl Iterator<Item = u64>,
    max_units: usize,
) -> Result<Option<u64>, String> {
    // One indexed probe replaces a COUNT for every candidate. A tail opening
    // at or before the 1251st newest event would exceed the existing ceiling.
    let head = i64::try_from(head).map_err(|_| "tail head exceeds SQLite range")?;
    let event_floor: Option<i64> = inbox
        .store()
        .connection()
        .query_row(
            "SELECT (SELECT seq FROM event
         WHERE aggregate_family = 'session' AND aggregate_id = ?1 AND seq <= ?2
         ORDER BY seq DESC LIMIT 1 OFFSET 1250)",
            (session.as_str(), head),
            |row| row.get(0),
        )
        .map_err(|error| format!("tail event bound is unreadable: {error}"))?;
    let mut removable = context
        .rows
        .iter()
        .filter(|(seq, _)| *seq != current)
        .try_fold(
            context.summary.as_ref().map_or(0, String::len),
            |bytes, (_, item)| {
                bytes
                    .checked_add(context_item_bytes(item))
                    .ok_or("working-set size overflowed")
            },
        )?;
    let mut rows = context.rows.iter().rev().peekable();
    let (mut bytes, mut units, mut items, mut calls, mut inputs) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut selected = None;
    for cut in candidates {
        let sql_cut = i64::try_from(cut).map_err(|_| "tail cut exceeds SQLite range")?;
        if event_floor.is_some_and(|floor| sql_cut <= floor) {
            break;
        }
        while rows.peek().is_some_and(|(seq, _)| *seq >= cut) {
            let Some((seq, item)) = rows.next() else {
                break;
            };
            let size = context_item_bytes(item);
            bytes = bytes.saturating_add(size);
            let charge = admission::item_charge(item)
                .map_err(|failure| format!("working-set charge failed: {failure:?}"))?;
            units = units.saturating_add(
                usize::try_from(charge).map_err(|_| "working-set charge exceeds usize")?,
            );
            items += 1;
            calls += usize::from(matches!(
                item,
                CodexInputItem::ToolCall { .. } | CodexInputItem::TextToolCall { .. }
            ));
            inputs += usize::from(matches!(
                item,
                CodexInputItem::Message(InputMessage {
                    role: MessageRole::User,
                    ..
                })
            ));
            if *seq != current {
                removable = removable
                    .checked_sub(size)
                    .ok_or("working-set size regressed")?;
            }
        }
        if units > max_units
            || bytes > 32 * 1024
            || items > MAX_CONTEXT_ITEMS / 4
            || calls > MAX_CONTINUATION_ROWS / 4
            || inputs > MAX_CONTINUATION_ROWS / 4
        {
            break;
        }
        // Keep some real prefix for the summary; a tail of the whole window
        // cannot make compaction progress.
        if removable == 0 {
            break;
        }
        selected = Some(cut);
    }
    Ok(selected)
}
