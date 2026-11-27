//! Transcript windows and individual projections have different query bounds.
use crate::write::EventStore;
use grapt_event::session_tool_terminal::StoredTerminal;
use grapt_types::{PermissionId, SessionId, ToolCallId};

pub mod batch;
pub mod boundary;
pub mod bounds;

#[derive(Debug)]
pub enum Scope<'a> {
    Call(Option<&'a ToolCallId>, &'static str),
    /// The replies to one permission ask.
    Request(&'a PermissionId),
    Composition(&'a ToolCallId),
    Model(Option<&'a ToolCallId>),
}

pub fn scoped_rows(
    store: &EventStore,
    session: &SessionId,
    kind: &str,
    from: Option<grapt_types::EventSeq>,
    until: Option<grapt_types::EventSeq>,
    scope: Scope<'_>,
    max_rows: usize,
) -> Result<Vec<(u64, String)>, String> {
    let window = Window {
        from,
        until,
        scope,
        max_rows,
    };
    scoped(store, session, kind, "e.data", window, |row| row.get(1))
}

/// One tool terminal a scoped read selected, classified by its decoder.
#[derive(Debug)]
pub struct ScopedTerminal {
    pub seq: u64,
    pub terminal: StoredTerminal,
    /// The row's call is the scope's target. SQLite compares it in place, so
    /// this holds for an identity of any length, even one an oversized row
    /// could not keep, and no row copies the target.
    pub names_target: bool,
}

/// [`scoped_rows`] for tool terminals, without fetching an oversized row or
/// failing on one that is not UTF-8.
pub fn scoped_terminals(
    store: &EventStore,
    session: &SessionId,
    from: Option<grapt_types::EventSeq>,
    until: Option<grapt_types::EventSeq>,
    scope: Scope<'_>,
    max_rows: usize,
) -> Result<Vec<ScopedTerminal>, String> {
    let window = Window {
        from,
        until,
        scope,
        max_rows,
    };
    let data = format!(
        "{}, json_type(e.data, '$.call') = 'text' AND json_extract(e.data, '$.call') = ?5",
        crate::stored_terminal::bounded_column("e.data")
    );
    let rows = scoped(
        store,
        session,
        "session.tool.terminal.1",
        &data,
        window,
        |row| {
            let terminal = crate::stored_terminal::classify(row, 1)?;
            Ok((terminal, row.get::<_, Option<bool>>(2)? == Some(true)))
        },
    )?;
    Ok(rows
        .into_iter()
        .map(|(seq, (terminal, names_target))| ScopedTerminal {
            seq,
            terminal,
            names_target,
        })
        .collect())
}

struct Window<'a> {
    from: Option<grapt_types::EventSeq>,
    until: Option<grapt_types::EventSeq>,
    scope: Scope<'a>,
    max_rows: usize,
}

fn scoped<T>(
    store: &EventStore,
    session: &SessionId,
    kind: &str,
    data: &str,
    window: Window<'_>,
    read: impl Fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<(u64, T)>, String> {
    let Window {
        from,
        until,
        scope,
        max_rows,
    } = window;
    let from = match from {
        Some(from) => {
            i64::try_from(from.value()).map_err(|_| "continuation cut exceeds SQLite range")?
        }
        None => i64::MIN,
    };
    let until = until
        .map(|until| {
            i64::try_from(until.value()).map_err(|_| "continuation window exceeds SQLite range")
        })
        .transpose()?;
    // Unclassifiable rows remain selected so the owning strict decoder refuses
    // corruption instead of silently treating it as unrelated or absent. A
    // tool terminal that is not JSON is the exception: it is no evidence for
    // any call (`StoredTerminal`), so no scope can claim it.
    let (target, path, predicate) = scope_selection(scope);
    let visible = crate::history_visibility::HistoryVisibility::read(store.connection(), session)
        .map_err(|error| error.to_string())?
        .sql("e.seq");
    let sql = scoped_sql(data, &predicate, "", &visible);
    let limit = i64::try_from(max_rows.saturating_add(1)).map_err(|_| "invalid row bound")?;
    let mut statement = store
        .connection()
        .prepare(&sql)
        .map_err(|e| e.to_string())?;
    let mapped = statement
        .query_map(
            (session.as_str(), kind, limit, from, target, path, until),
            |row| Ok((row.get::<_, i64>(0)?, read(row)?)),
        )
        .map_err(|e| format!("continuation query failed: {e}"))?;
    let mut rows = Vec::new();
    for row in mapped {
        let (seq, data) = row.map_err(|e| e.to_string())?;
        let seq = u64::try_from(seq).map_err(|_| "continuation row has negative sequence")?;
        rows.push((seq, data));
    }
    if rows.len() > max_rows {
        return Err(format!("{kind} exceeds the continuation row bound"));
    }
    Ok(rows)
}

fn call_predicate(comparison: &str) -> String {
    format!("?5 IS NULL OR json_type(e.data, ?6) IS NOT 'text' OR json_extract(e.data, ?6) {comparison}")
}

/// A model terminal is one no nested start shares (attempt, call) with. The
/// nested pairs are one uncorrelated set, read once per statement: as a
/// correlated NOT EXISTS it walked every nested start for every terminal, 0.4 s
/// per tool part on row A (SC-HIST-01). NULL pairs match nothing, as `=` did,
/// so they are kept out of the set and a terminal without a pair stays model.
fn model_predicate(comparison: &str) -> String {
    format!("(?5 IS NULL OR json_type(e.data, '$.call') IS NOT 'text' OR json_extract(e.data, '$.call') {comparison})
             AND CASE WHEN json_extract(e.data, '$.attempt') IS NULL
                     OR json_extract(e.data, '$.call') IS NULL THEN 1
                 ELSE (json_extract(e.data, '$.attempt'), json_extract(e.data, '$.call')) NOT IN (
                     SELECT json_extract(n.data, '$.attempt'), json_extract(n.data, '$.call')
                     FROM event n WHERE n.aggregate_family = 'session'
                     AND n.aggregate_id = ?1 AND n.type = 'session.tool.nested.started.1'
                     AND json_valid(n.data)
                     AND json_extract(n.data, '$.attempt') IS NOT NULL
                     AND json_extract(n.data, '$.call') IS NOT NULL) END")
}

fn scope_selection(scope: Scope<'_>) -> (Option<&str>, &'static str, String) {
    match scope {
        Scope::Call(target, path) => (target.map(ToolCallId::as_str), path, call_predicate("= ?5")),
        Scope::Request(request) => (
            Some(request.as_str()),
            "$.requestID",
            "json_type(e.data, ?6) IS NOT 'text' OR json_extract(e.data, ?6) = ?5".to_owned(),
        ),
        Scope::Composition(parent) => (
            Some(parent.as_str()),
            "$.call",
            // The parent's nested (attempt, call) pairs are one uncorrelated set,
            // read once. As a correlated EXISTS it walked every nested start for
            // every terminal in the Session: 0.39 s per batch-tool part on row A,
            // 15 s for one 50-message page (SC-HIST-01).
            "json_type(e.data, '$.call') IS NOT 'text' OR json_extract(e.data, '$.call') = ?5
             OR (json_extract(e.data, '$.attempt'), json_extract(e.data, '$.call')) IN (
                 SELECT json_extract(n.data, '$.attempt'), json_extract(n.data, '$.call')
                 FROM event n WHERE n.aggregate_family = 'session'
                 AND n.aggregate_id = ?1 AND n.type = 'session.tool.nested.started.1'
                 AND json_valid(n.data) AND json_extract(n.data, '$.parent') = ?5)"
                .to_owned(),
        ),
        Scope::Model(target) => (
            target.map(ToolCallId::as_str),
            "$.call",
            model_predicate("= ?5"),
        ),
    }
}

fn scoped_sql(data: &str, predicate: &str, extra_columns: &str, visible: &str) -> String {
    format!(
        "SELECT e.seq, {data}{extra_columns} FROM event e
         WHERE e.aggregate_family = 'session' AND e.aggregate_id = ?1 AND {visible} AND e.type = ?2
         AND (e.seq >= ?4 OR e.seq < 0) AND ?6 IS NOT NULL
         AND (?7 IS NULL OR e.seq < ?7)
         AND CASE WHEN json_valid(e.data) = 0 THEN e.type != 'session.tool.terminal.1'
              ELSE ({predicate}) END
         ORDER BY e.seq LIMIT ?3"
    )
}

#[cfg(test)]
#[path = "session_event_queries/batch_tests.rs"]
mod batch_tests;
