//! Conversation cuts are derived from durable revert facts. Operational facts
//! remain intact; transcript readers apply these ranges before their row bounds.

use std::collections::BTreeMap;

use grapt_types::{EventSeq, MessageId, SessionId};
use rusqlite::Connection;

pub const MAX_REWINDS: usize = 256;

/// The revert boundaries of one session. Every transcript read asks for
/// these, a history page once per row, so the read must not visit the whole
/// session. The match stays case-insensitive LIKE: a case-variant boundary is
/// selected and then refused as unsupported, never silently dropped. LIKE
/// cannot range over `event_aggregate_type_seq_idx`, so the session's distinct
/// types are walked on that index instead (one seek per type), the pattern is
/// applied to those few, and only matching types are read by `type = ?`.
const BOUNDARIES: &str = "WITH RECURSIVE kinds(type) AS (
       SELECT min(type) FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
       UNION ALL
       SELECT (SELECT min(type) FROM event
               WHERE aggregate_family = 'session' AND aggregate_id = ?1 AND type > kinds.type)
       FROM kinds WHERE kinds.type IS NOT NULL),
     boundary(type) AS (SELECT type FROM kinds WHERE type LIKE 'session.revert.committed.%')
     SELECT event.seq, event.type,
       CASE WHEN length(CAST(event.data AS BLOB)) <= 4096 THEN event.data END
     FROM boundary CROSS JOIN event
     WHERE event.aggregate_family = 'session' AND event.aggregate_id = ?1
       AND event.type = boundary.type
     ORDER BY event.seq LIMIT ?2";

#[derive(Debug, thiserror::Error)]
pub enum VisibilityError {
    #[error("rewind history cannot be read: {0}")]
    Store(#[from] rusqlite::Error),
    #[error("rewind history is corrupt: {0}")]
    Corrupt(String),
    #[error("session exceeds its {MAX_REWINDS} conversation rewind limit")]
    TooLong,
}

#[derive(Debug, Default)]
pub struct HistoryVisibility {
    cuts: Vec<(u64, u64)>,
}

impl HistoryVisibility {
    /// Reconstruct cuts from canonical events, independently of projection caches.
    pub fn read(conn: &Connection, session: &SessionId) -> Result<Self, VisibilityError> {
        let mut statement = conn.prepare(BOUNDARIES)?;
        let limit = i64::try_from(MAX_REWINDS + 1).map_err(|_| {
            VisibilityError::Corrupt("rewind query limit exceeds SQLite integers".into())
        })?;
        let boundaries = statement
            .query_map((session.as_str(), limit), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        // Every read path asks for visibility, so one promotion query serves
        // all cuts; a query per cut made each read grow with cuts × prompts.
        let promotions = Promotions::read(conn, session, &boundaries)?;
        let mut result = Self::default();
        for (end, kind, raw) in boundaries {
            if result.cuts.len() == MAX_REWINDS {
                return Err(VisibilityError::TooLong);
            }
            if kind != "session.revert.committed.1" {
                return Err(VisibilityError::Corrupt(format!(
                    "unsupported boundary {kind}"
                )));
            }
            let raw =
                raw.ok_or_else(|| VisibilityError::Corrupt("boundary exceeds 4096 bytes".into()))?;
            let fact = boundary(&raw)?;
            if fact.session() != session {
                return Err(VisibilityError::Corrupt(
                    "boundary names another session".into(),
                ));
            }
            let end = sequence(end)?;
            let from = promotions.sequence(session, fact.before())?.value();
            if from >= end || !result.contains(from) {
                return Err(VisibilityError::Corrupt(
                    "boundary does not select a surviving earlier prompt".into(),
                ));
            }
            result.cuts.push((from, end));
        }
        Ok(result)
    }

    pub fn contains(&self, seq: u64) -> bool {
        !self
            .cuts
            .iter()
            .any(|(from, end)| (*from..*end).contains(&seq))
    }

    pub fn count(&self) -> usize {
        self.cuts.len()
    }

    /// `column` must be a fixed, caller-owned SQL column expression. Only
    /// validated integer sequences enter the returned predicate.
    pub fn sql(&self, column: &str) -> String {
        if self.cuts.is_empty() {
            return "1".into();
        }
        // A later rewind can contain older cuts. Normalize their union for
        // lookup only; the original facts still govern validation and count.
        let mut cuts = self.cuts.clone();
        cuts.sort_unstable();
        let mut ranges: Vec<(u64, u64)> = Vec::with_capacity(cuts.len());
        for (from, end) in cuts {
            match ranges.last_mut() {
                Some(last) if from <= last.1 => last.1 = last.1.max(end),
                _ => ranges.push((from, end)),
            }
        }
        visible_sql(column, &ranges)
    }
}

/// SQLite CASE evaluates only the chosen branch. A flat OR tested every cut
/// for each surviving row, multiplying transcript reads by the rewind count.
fn visible_sql(column: &str, ranges: &[(u64, u64)]) -> String {
    let middle = ranges.len() / 2;
    let Some((from, end)) = ranges.get(middle) else {
        return "1".into();
    };
    let before = visible_sql(column, &ranges[..middle]);
    let after = visible_sql(column, &ranges[middle + 1..]);
    format!(
        "(CASE WHEN {column} < {from} THEN {before} WHEN {column} >= {end} THEN {after} ELSE 0 END)"
    )
}

/// Resolve the immutable promotion behind a selected user message. Duplicate
/// promotions and malformed matching records refuse instead of choosing one.
pub fn promotion_sequence(
    conn: &Connection,
    session: &SessionId,
    input: &MessageId,
) -> Result<EventSeq, VisibilityError> {
    let mut statement = conn.prepare(
        "SELECT seq, CASE WHEN length(CAST(data AS BLOB)) <= 4096 THEN data END
         FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
         AND type = 'session.input.promoted.1' AND CASE WHEN json_valid(data) = 0 THEN 1
           WHEN json_type(data, '$.inputID') IS NOT 'text' THEN 1
           ELSE json_extract(data, '$.inputID') = ?2 END ORDER BY seq LIMIT 2",
    )?;
    let mut rows = statement.query((session.as_str(), input.as_str()))?;
    let first = match rows.next()? {
        Some(row) => Some((row.get(0)?, row.get(1)?)),
        None => None,
    };
    let seq = promoted(session, input, first)?;
    if rows.next()?.is_some() {
        return Err(duplicate());
    }
    Ok(seq)
}

/// The promotions every cut of one read selects, gathered in one query. Each
/// input keeps the first two records [`promotion_sequence`] would have matched
/// for it — its own and any malformed one — so both refuse identically.
struct Promotions {
    matches: BTreeMap<String, Vec<(i64, Option<String>)>>,
}

impl Promotions {
    fn read(
        conn: &Connection,
        session: &SessionId,
        boundaries: &[(i64, String, Option<String>)],
    ) -> Result<Self, VisibilityError> {
        let mut matches: BTreeMap<String, Vec<(i64, Option<String>)>> = boundaries
            .iter()
            .filter(|(_, kind, _)| kind == "session.revert.committed.1")
            .filter_map(|(_, _, raw)| boundary(raw.as_deref()?).ok())
            .map(|fact| (fact.before().as_str().to_owned(), Vec::new()))
            .collect();
        if matches.is_empty() {
            return Ok(Self { matches });
        }
        let wanted = serde_json::to_string(&matches.keys().collect::<Vec<_>>())
            .map_err(|error| VisibilityError::Corrupt(error.to_string()))?;
        let mut statement = conn.prepare(
            "SELECT seq, input, raw FROM (
               SELECT seq, CASE WHEN json_valid(data) = 0 THEN NULL
                   WHEN json_type(data, '$.inputID') IS NOT 'text' THEN NULL
                   ELSE json_extract(data, '$.inputID') END AS input,
                 CASE WHEN length(CAST(data AS BLOB)) <= 4096 THEN data END AS raw
               FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type = 'session.input.promoted.1')
             WHERE input IS NULL OR input IN (SELECT value FROM json_each(?2))
             ORDER BY seq",
        )?;
        let mut rows = statement.query((session.as_str(), wanted))?;
        let mut open = matches.len();
        while open > 0 {
            let Some(row) = rows.next()? else { break };
            let (seq, input, raw): (i64, Option<String>, Option<String>) =
                (row.get(0)?, row.get(1)?, row.get(2)?);
            let found: Vec<&mut Vec<_>> = match input {
                Some(input) => matches.get_mut(&input).into_iter().collect(),
                None => matches.values_mut().collect(),
            };
            for found in found.into_iter().filter(|found| found.len() < 2) {
                found.push((seq, raw.clone()));
                if found.len() == 2 {
                    open -= 1;
                }
            }
        }
        Ok(Self { matches })
    }

    fn sequence(
        &self,
        session: &SessionId,
        input: &MessageId,
    ) -> Result<EventSeq, VisibilityError> {
        let mut found = self
            .matches
            .get(input.as_str())
            .into_iter()
            .flatten()
            .cloned();
        let seq = promoted(session, input, found.next())?;
        if found.next().is_some() {
            return Err(duplicate());
        }
        Ok(seq)
    }
}

fn promoted(
    session: &SessionId,
    input: &MessageId,
    row: Option<(i64, Option<String>)>,
) -> Result<EventSeq, VisibilityError> {
    let (seq, raw) =
        row.ok_or_else(|| VisibilityError::Corrupt("selected prompt has no promotion".into()))?;
    let seq = sequence(seq)?;
    let raw = raw.ok_or_else(|| VisibilityError::Corrupt("promotion exceeds 4096 bytes".into()))?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|error| VisibilityError::Corrupt(error.to_string()))?;
    if value.get("sessionID").and_then(serde_json::Value::as_str) != Some(session.as_str())
        || value.get("inputID").and_then(serde_json::Value::as_str) != Some(input.as_str())
    {
        return Err(VisibilityError::Corrupt(
            "promotion has different input coordinates".into(),
        ));
    }
    Ok(EventSeq::new(seq))
}

fn duplicate() -> VisibilityError {
    VisibilityError::Corrupt("selected prompt has duplicate promotions".into())
}

fn boundary(
    raw: &str,
) -> Result<grapt_event::session_revert::RevertCommittedFact, VisibilityError> {
    let value =
        serde_json::from_str(raw).map_err(|error| VisibilityError::Corrupt(error.to_string()))?;
    grapt_event::session_revert::decode(&value)
        .map_err(|error| VisibilityError::Corrupt(error.to_string()))
}

fn sequence(raw: i64) -> Result<u64, VisibilityError> {
    u64::try_from(raw).map_err(|_| VisibilityError::Corrupt("negative sequence".into()))
}

#[cfg(test)]
mod tests;
