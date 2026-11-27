//! SC-REL-10: a recent window past its materialization budget is read from
//! a later exchange opening, publishing the value the whole window gives.
use super::*;

/// The public recent value of the window [from, head], read whole when it
/// fits. Past the budget the read starts at the earliest promoted user
/// input that fits instead: the published half is the same as long as its
/// split falls after that start, and otherwise the latest exchange alone
/// overflows, which is refused rather than truncated.
pub(super) fn window_recent(
    inbox: &Inbox,
    artifacts: &FilesystemArtifactStore,
    session: &SessionId,
    from: u64,
    head: u64,
    previous_recent: &Utf16Text,
) -> Result<Utf16Text, String> {
    match conversation_entries(inbox, artifacts, session, from, head) {
        Ok(entries) => return Ok(select_recent(&entries, KEEP_TOKENS, previous_recent)?.2),
        Err(WindowError::Capacity(_)) => {}
        Err(WindowError::Failed(detail)) => return Err(detail),
    }
    let starts = user_inputs_after(inbox.store(), session, from, head)?;
    // Fitting is monotone in the start: a later one reads a subset.
    let (mut low, mut high) = (0, starts.len());
    let mut fitted = None;
    while low < high {
        let middle = low + (high - low) / 2;
        match conversation_entries(inbox, artifacts, session, starts[middle], head) {
            Ok(entries) => {
                fitted = Some(entries);
                high = middle;
            }
            Err(WindowError::Capacity(_)) => low = middle + 1,
            Err(WindowError::Failed(detail)) => return Err(detail),
        }
    }
    // The window opens on a user input, so a pullback stops there at the
    // latest. Only a walk that keeps every entry could have gone further
    // back into what was not read.
    let entries = fitted
        .filter(|entries| {
            entries
                .iter()
                .map(|entry| estimate_tokens(&entry.text))
                .fold(0u64, u64::saturating_add)
                > KEEP_TOKENS
        })
        .ok_or("the latest recent exchange exceeds the request context bound")?;
    // The unread history before the start is a nonempty head, so neither
    // the latest-user rule nor the summarize-recent branch applies.
    publish(&join_entry_units(
        &entries[walk_split(&entries, KEEP_TOKENS)..],
    ))
}

/// The visible user promoted inputs in (from, head], in sequence order:
/// the exchange openings a bounded window may start at.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(in crate::dogfood::continuation) fn user_inputs_after(
    store: &EventStore,
    session: &SessionId,
    from: u64,
    head: u64,
) -> Result<Vec<u64>, String> {
    let visible =
        grapt_store::history_visibility::HistoryVisibility::read(store.connection(), session)
            .map_err(|error| error.to_string())?
            .sql("seq");
    let from =
        i64::try_from(from).map_err(|_| "recent window start exceeds SQLite range".to_owned())?;
    let head =
        i64::try_from(head).map_err(|_| "recent window head exceeds SQLite range".to_owned())?;
    let mut statement = store
        .connection()
        .prepare(&format!(
            "SELECT seq FROM session_message
             WHERE session_id = ?1 AND type = 'user' AND {visible} AND seq > ?2 AND seq <= ?3
             ORDER BY seq ASC LIMIT {}",
            MAX_RECENT_WINDOW_ROWS + 1
        ))
        .map_err(|error| error.to_string())?;
    let starts = statement
        .query_map((session.as_str(), from, head), |row| row.get::<_, i64>(0))
        .map_err(|error| error.to_string())?
        .map(|seq| {
            let seq = seq.map_err(|error| error.to_string())?;
            u64::try_from(seq)
                .map_err(|_| format!("recent promoted message sequence {seq} is negative"))
        })
        .collect::<Result<Vec<_>, String>>()?;
    if starts.len() > MAX_RECENT_WINDOW_ROWS {
        return Err(format!(
            "recent promoted inputs exceed their {MAX_RECENT_WINDOW_ROWS} bound"
        ));
    }
    // An input promoted while a response was still open (its message had
    // more content, or a call its terminal, after the input) is inside that
    // exchange: a window starting there would hold half of it.
    // The identities are DB-controlled text: each is measured before it is
    // fetched, and what is fetched is charged against the context budget.
    let terminal = grapt_store::stored_terminal::bounded_column("data");
    let mut statement = store
        .connection()
        .prepare(&format!(
            "SELECT seq, type, length(CAST(message AS BLOB)), CASE WHEN length(CAST(message AS BLOB)) <= {MAX_RECENT_ID_BYTES} THEN message END,
                    length(CAST(call AS BLOB)), CASE WHEN length(CAST(call AS BLOB)) <= {MAX_RECENT_ID_BYTES} THEN call END,
                    terminal
             FROM (SELECT seq, type,
                          -- A terminal's identity comes from its decoded fact
                          -- (`StoredTerminal`), never from its raw fields.
                          CASE WHEN type != 'session.tool.terminal.1'
                               THEN json_extract(data, '$.message') END AS message,
                          CASE WHEN type != 'session.tool.terminal.1'
                               THEN COALESCE(json_extract(data, '$.item.call'),
                                             json_extract(data, '$.call')) END AS call,
                          CASE WHEN type = 'session.tool.terminal.1'
                               THEN {terminal} END AS terminal
                   FROM event
                   WHERE aggregate_family = 'session' AND aggregate_id = ?1 AND {visible}
                   AND seq > ?2 AND seq <= ?3 AND json_valid(data)
                   AND type IN ('session.assistant-message.opened.1',
                                'session.assistant-message.opened.2',
                                'session.content.appended.1', 'session.tool.terminal.1')
                   ORDER BY seq ASC LIMIT {})",
            MAX_RECENT_WINDOW_ROWS + 1
        ))
        .map_err(|error| error.to_string())?;
    let mut rows = Vec::new();
    let mut budget = MAX_CONTEXT_BYTES;
    let mut cursor = statement
        .query((session.as_str(), from, head))
        .map_err(|error| error.to_string())?;
    while let Some(row) = cursor.next().map_err(|error| error.to_string())? {
        if rows.len() == MAX_RECENT_WINDOW_ROWS {
            return Err(format!(
                "recent exchange rows exceed their {MAX_RECENT_WINDOW_ROWS} bound"
            ));
        }
        let mut identity = |length: usize, value: usize| -> Result<Option<String>, String> {
            let length: Option<i64> = row.get(length).map_err(|error| error.to_string())?;
            let Some(length) = length else {
                return Ok(None);
            };
            if length > MAX_RECENT_ID_BYTES as i64 {
                return Err(format!(
                    "recent exchange identity exceeds its {MAX_RECENT_ID_BYTES}-byte read bound"
                ));
            }
            charge_recent(
                &mut budget,
                usize::try_from(length)
                    .map_err(|_| "recent exchange identity length is negative")?,
                "recent exchange identities exceed the request context bound",
            )?;
            row.get(value).map_err(|error| error.to_string())
        };
        let (message, mut call) = (identity(2, 3)?, identity(4, 5)?);
        // An unreadable or oversized terminal names no call.
        let terminal = grapt_store::stored_terminal::classify_optional(row, 6)
            .map_err(|error| error.to_string())?;
        if let Some(StoredTerminal::Readable(fact)) = terminal {
            let named = fact.call().as_str();
            charge_recent(
                &mut budget,
                named.len(),
                "recent exchange identities exceed the request context bound",
            )?;
            call = Some(named.to_owned());
        }
        rows.push((
            row.get::<_, i64>(0).map_err(|error| error.to_string())?,
            row.get::<_, String>(1).map_err(|error| error.to_string())?,
            message,
            call,
        ));
    }
    // Each message spans its opening to its last content, and each call its
    // content to its terminal.
    let mut spans: HashMap<(bool, String), (i64, i64)> = HashMap::new();
    for (seq, kind, message, call) in rows {
        let terminal = kind == "session.tool.terminal.1";
        if let Some(message) = message.filter(|_| !terminal) {
            spans.entry((false, message)).or_insert((seq, seq)).1 = seq;
        }
        if let Some(call) = call.filter(|_| terminal || kind == "session.content.appended.1") {
            spans.entry((true, call)).or_insert((seq, seq)).1 = seq;
        }
    }
    let mut inside = vec![0i64; starts.len() + 1];
    for (opened, closed) in spans.into_values() {
        let (Ok(opened), Ok(closed)) = (u64::try_from(opened), u64::try_from(closed)) else {
            return Err("recent exchange sequence is negative".to_owned());
        };
        let first = starts.partition_point(|start| *start <= opened);
        let last = starts.partition_point(|start| *start <= closed);
        if first < last {
            inside[first] += 1;
            inside[last] -= 1;
        }
    }
    let mut depth = 0i64;
    Ok(starts
        .into_iter()
        .zip(inside)
        .filter_map(|(start, change)| {
            depth += change;
            (depth == 0).then_some(start)
        })
        .collect())
}
