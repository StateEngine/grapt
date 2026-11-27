use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

use grapt_store::commit_projection::ProjectionConnection;
use grapt_store::read_snapshot::ReadSnapshot;
use grapt_types::SessionId;

use super::{CleanupCommand, CleanupDisposition, CleanupReport, RetentionReason, SessionDecision};

mod lifecycle;
mod tools;
mod work;
pub(super) use lifecycle::SessionEvidence;

const MAX_ROWS: usize = 100_000;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_SESSIONS: usize = 10_000;

#[derive(Debug)]
enum Reader<'a> {
    Connection(&'a ProjectionConnection),
    Snapshot(&'a ReadSnapshot<'a>),
}

#[derive(Debug)]
pub(super) struct Source<'a> {
    reader: Reader<'a>,
    rows: Cell<usize>,
    bytes: Cell<usize>,
    statements: Cell<usize>,
}

impl<'a> Source<'a> {
    pub(super) fn connection(connection: &'a ProjectionConnection) -> Self {
        Self::new(Reader::Connection(connection))
    }

    pub(super) fn snapshot(snapshot: &'a ReadSnapshot<'a>) -> Self {
        Self::new(Reader::Snapshot(snapshot))
    }

    fn new(reader: Reader<'a>) -> Self {
        Self {
            reader,
            rows: Cell::new(0),
            bytes: Cell::new(0),
            statements: Cell::new(0),
        }
    }

    fn charge_statement(&self) -> Result<(), String> {
        self.statements.set(self.statements.get() + 1);
        if self.statements.get() > 128 {
            return Err("cleanup statement budget exceeded; nothing may be removed".into());
        }
        Ok(())
    }

    /// All queries bound returned cells in SQL before materializing them.
    pub(super) fn select<const N: usize>(
        &self,
        sql: &str,
        mut visit: impl FnMut([Option<String>; N]) -> Result<(), String>,
    ) -> Result<(), String> {
        self.select_rows(sql, false, |cells, _| visit(cells))
    }

    /// The final column is a SQL-size-bounded canonical binary record.
    pub(super) fn records<const N: usize>(
        &self,
        sql: &str,
        visit: impl FnMut([Option<String>; N], Option<Vec<u8>>) -> Result<(), String>,
    ) -> Result<(), String> {
        self.select_rows(sql, true, visit)
    }

    fn select_rows<const N: usize>(
        &self,
        sql: &str,
        has_record: bool,
        mut visit: impl FnMut([Option<String>; N], Option<Vec<u8>>) -> Result<(), String>,
    ) -> Result<(), String> {
        self.charge_statement()?;
        let mut statement = match self.reader {
            Reader::Connection(connection) => connection.prepare(sql),
            Reader::Snapshot(snapshot) => snapshot.prepare(sql),
        }
        .map_err(|error| error.to_string())?;
        let mut rows = statement.query([]).map_err(|error| error.to_string())?;
        while let Some(row) = rows.next().map_err(|error| error.to_string())? {
            self.rows.set(self.rows.get() + 1);
            if self.rows.get() > MAX_ROWS {
                return Err("cleanup row budget exceeded; nothing may be removed".into());
            }
            let mut cells = std::array::from_fn(|_| None);
            for (index, cell) in cells.iter_mut().enumerate() {
                *cell = row
                    .get::<_, Option<String>>(index)
                    .map_err(|error| error.to_string())?;
                self.bytes
                    .set(self.bytes.get() + cell.as_ref().map_or(0, String::len));
            }
            let record = if has_record {
                row.get::<_, Option<Vec<u8>>>(N)
                    .map_err(|error| error.to_string())?
            } else {
                None
            };
            self.bytes
                .set(self.bytes.get() + record.as_ref().map_or(0, Vec::len));
            if self.bytes.get() > MAX_BYTES {
                return Err("cleanup byte budget exceeded; nothing may be removed".into());
            }
            visit(cells, record)?;
        }
        Ok(())
    }

    pub(super) fn columns(&self, table: &str) -> Result<Vec<String>, String> {
        self.charge_statement()?;
        // Names were bounded/validated during schema discovery. This inspects
        // metadata only, including views; no PRAGMA or owner initialization.
        let sql = format!("SELECT * FROM \"{table}\" LIMIT 0");
        let statement = match self.reader {
            Reader::Connection(connection) => connection.prepare(&sql),
            Reader::Snapshot(snapshot) => snapshot.prepare(&sql),
        }
        .map_err(|error| error.to_string())?;
        if statement.column_count() > 128 || statement.column_names().any(|name| name.len() > 128) {
            return Err("cleanup column metadata budget exceeded".into());
        }
        Ok(statement.column_names().map(str::to_owned).collect())
    }
}

pub(super) fn required(value: Option<String>) -> Result<String, String> {
    value.ok_or_else(|| "cleanup encountered missing or oversized evidence".into())
}

pub(super) fn scan(source: &Source<'_>, command: &CleanupCommand) -> Result<CleanupReport, String> {
    let mut sessions: BTreeMap<SessionId, SessionEvidence> = BTreeMap::new();
    source.select(
        "SELECT CASE WHEN length(CAST(aggregate_id AS BLOB))<=512 THEN aggregate_id END,
         CAST(seq AS TEXT), CAST(created AS TEXT),
         CASE WHEN length(CAST(type AS BLOB))<=256 THEN type END,
         CASE WHEN length(CAST(data AS BLOB))<=65536 THEN data END,
         CASE WHEN length(CAST(id AS BLOB))<=512 THEN id END
         FROM event WHERE aggregate_family='session' ORDER BY aggregate_id,seq LIMIT 100001",
        |[session, seq, created, kind, data, event]| {
            let session: SessionId = required(session)?
                .parse()
                .map_err(|error| format!("invalid session: {error}"))?;
            let seq: u64 = required(seq)?
                .parse()
                .map_err(|_| "invalid event sequence")?;
            let created: i64 = required(created)?
                .parse()
                .map_err(|_| "invalid event timestamp")?;
            let _: grapt_types::EventId = required(event)?
                .parse()
                .map_err(|error| format!("invalid event id: {error}"))?;
            sessions.entry(session.clone()).or_default().observe(
                &session,
                (seq, created),
                &required(kind)?,
                data,
                command.cutoff.as_millis(),
            )?;
            if sessions.len() > MAX_SESSIONS {
                return Err("cleanup session budget exceeded".into());
            }
            Ok(())
        },
    )?;
    let mut cursors = BTreeSet::new();
    source.select(
        "SELECT CASE WHEN length(CAST(aggregate_id AS BLOB))<=512 THEN aggregate_id END,
         CAST(seq AS TEXT) FROM event_sequence WHERE aggregate_family='session' LIMIT 100001",
        |[session, seq]| {
            let session: SessionId = required(session)?
                .parse()
                .map_err(|error| format!("invalid cursor session: {error}"))?;
            let seq: u64 = required(seq)?
                .parse()
                .map_err(|_| "invalid cursor sequence")?;
            if sessions.get(&session).and_then(|state| state.seq) != Some(seq) {
                return Err("session cursor and durable event tail disagree".into());
            }
            cursors.insert(session);
            Ok(())
        },
    )?;
    if cursors.len() != sessions.len() {
        return Err("session event history has no matching cursor".into());
    }
    for state in sessions.values_mut() {
        state.finish()?;
    }
    // Inspect EVERY seed, not just live/listed seeds: tombstoned children still
    // protect their parent. Any direct child suffices to make an ancestor ineligible.
    let parents: Vec<SessionId> = sessions
        .values()
        .filter_map(|state| state.parent.clone())
        .collect();
    for parent in parents {
        if let Some(state) = sessions.get_mut(&parent) {
            state.retain(RetentionReason::Family);
        }
    }
    super::tables::protect(source, &mut sessions, command.cutoff.as_millis())?;
    let sessions = sessions
        .into_iter()
        .map(|(session, state)| {
            let reason = if session == command.current {
                Some(RetentionReason::CurrentSession)
            } else if state.last_event_ms >= command.cutoff.as_millis() {
                Some(RetentionReason::NotOld)
            } else if state.deleted {
                Some(RetentionReason::Deleted)
            } else if !state.seeded || state.open_execution.is_some() {
                Some(RetentionReason::IncompleteExecution)
            } else {
                state.retained
            };
            SessionDecision {
                session,
                last_event_ms: state.last_event_ms,
                disposition: reason
                    .map_or(CleanupDisposition::Eligible, CleanupDisposition::Retained),
            }
        })
        .collect();
    Ok(CleanupReport {
        applied: false,
        sessions,
    })
}

pub(super) fn protect_session(
    sessions: &mut BTreeMap<SessionId, SessionEvidence>,
    raw: String,
) -> Result<(), String> {
    let session: SessionId = raw
        .parse()
        .map_err(|error| format!("invalid operational session: {error}"))?;
    let state = sessions
        .get_mut(&session)
        .ok_or("operational evidence names a session without inspectable history")?;
    state.retain(RetentionReason::OperationalEvidence);
    Ok(())
}

pub(super) fn protect_all(sessions: &mut BTreeMap<SessionId, SessionEvidence>) {
    for state in sessions.values_mut() {
        state.retain(RetentionReason::UnattributedWork);
    }
}
