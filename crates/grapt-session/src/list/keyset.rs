//! Where a session-list page continues. Rows are totally ordered by
//! `(updated_ms, key present, key, rowid)`: within one time, keyless rows
//! (by `rowid`) sort before keyed ones, matching SQLite's own NULL order in
//! the key index. Continuing after a position reads a short sequence of
//! segments, each a single index seek, so a page costs the same however deep
//! into a run of rows sharing one time it starts.
use rusqlite::types::Value as SqlValue;

/// A position the list continues strictly after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum After {
    /// A listed Session, by its time and key.
    Session { time: i64, id: String },
    /// A stored row, by `rowid`; `keyed` when its key is not NULL.
    Row { rowid: i64, keyed: bool },
}

/// One seekable run of the keyset, in the page's physical order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Segment {
    /// The whole list from its first row.
    All,
    /// Keyed rows at the position's time, past its key when `past`.
    Keyed { at: After, past: bool },
    /// Keyless rows at the position's time, past its `rowid` when `past`.
    Keyless { at: After, past: bool },
    /// Rows at later times, or earlier ones when descending.
    Beyond { at: After },
}

/// The segments after `after`, in order, for an ascending or descending read.
pub(super) fn segments(after: Option<&After>, descending: bool) -> Vec<Segment> {
    let Some(at) = after else {
        return vec![Segment::All];
    };
    let keyed = match at {
        After::Session { .. } => true,
        After::Row { keyed, .. } => *keyed,
    };
    let past = |at: &After| Segment::Keyed {
        at: at.clone(),
        past: true,
    };
    let beyond = Segment::Beyond { at: at.clone() };
    match (keyed, descending) {
        (true, false) => vec![past(at), beyond],
        (true, true) => vec![
            past(at),
            Segment::Keyless {
                at: at.clone(),
                past: false,
            },
            beyond,
        ],
        (false, false) => vec![
            Segment::Keyless {
                at: at.clone(),
                past: true,
            },
            Segment::Keyed {
                at: at.clone(),
                past: false,
            },
            beyond,
        ],
        (false, true) => vec![
            Segment::Keyless {
                at: at.clone(),
                past: true,
            },
            beyond,
        ],
    }
}

impl Segment {
    /// The `WHERE` terms that select this segment. `comparison` is `>` when
    /// ascending and `<` when descending; `push` binds a value and names it.
    pub(super) fn terms(
        &self,
        comparison: &str,
        push: &mut impl FnMut(SqlValue) -> String,
    ) -> String {
        match self {
            Self::All => String::new(),
            Self::Keyed { at, past } => {
                let time = time(at, push);
                let mut terms = format!(" AND updated_ms = {time} AND session_id IS NOT NULL");
                if *past {
                    // Keys are unique, so the key alone places a keyed row.
                    let key = key(at, push);
                    terms.push_str(&format!(
                        " AND session_id COLLATE BINARY {comparison} {key}"
                    ));
                }
                terms
            }
            Self::Keyless { at, past } => {
                let time = time(at, push);
                let mut terms = format!(" AND updated_ms = {time} AND session_id IS NULL");
                if *past {
                    let rowid = rowid(at, push);
                    terms.push_str(&format!(" AND rowid {comparison} {rowid}"));
                }
                terms
            }
            Self::Beyond { at } => {
                let time = time(at, push);
                format!(" AND updated_ms {comparison} {time}")
            }
        }
    }
}

// The cursor's time and key are read exactly, whatever their size: a bounded
// read would lose the position and end the list early. An oversized key is
// loaded in full by any index seek that compares against it (SQLite's
// `sqlite3BtreeIndexMoveto` reads every overflow cell it compares), so the
// width bound belongs in the schema as a length `CHECK`, tracked for Phase 2
// (G2.2), not in these lookups.
fn time(at: &After, push: &mut impl FnMut(SqlValue) -> String) -> String {
    match at {
        After::Session { time, .. } => push(SqlValue::Integer(*time)),
        After::Row { rowid, .. } => {
            let rowid = push(SqlValue::Integer(*rowid));
            format!("(SELECT updated_ms FROM session_list_projection WHERE rowid = {rowid})")
        }
    }
}

fn key(at: &After, push: &mut impl FnMut(SqlValue) -> String) -> String {
    match at {
        After::Session { id, .. } => push(SqlValue::Text(id.clone())),
        After::Row { rowid, .. } => {
            let rowid = push(SqlValue::Integer(*rowid));
            format!("(SELECT session_id FROM session_list_projection WHERE rowid = {rowid})")
        }
    }
}

/// A Session position has no `rowid`; `NULL` compares to nothing.
fn rowid(at: &After, push: &mut impl FnMut(SqlValue) -> String) -> String {
    match at {
        After::Session { .. } => "NULL".to_owned(),
        After::Row { rowid, .. } => push(SqlValue::Integer(*rowid)),
    }
}
