//! Read the usage observations of one exact physical attempt.
//!
//! This is its own bounded read, not a windowed fold: an attempt's observations
//! are found by the attempt identity in the payload, so the probe never depends
//! on Session lifetime and one long conversation cannot force a lifetime fold
//! to answer a per-attempt question.
use super::*;

/// Durable usage rows one attempt may have before the read refuses.
const MAX_ATTEMPT_USAGE_ROWS: usize = 64;

impl SessionHistoryStore<'_> {
    /// The usage observations of one exact physical attempt, with the gaps the
    /// read left behind.
    ///
    /// An unreadable usage row costs that observation and is named, so a caller
    /// that reports evidence can say exactly which durable rows it could not
    /// read instead of silently returning a shorter list. There is deliberately
    /// no variant that drops the gaps: a shorter list must never be answered as
    /// a complete one.
    pub fn usage_for_attempt(
        &self,
        session: &SessionId,
        attempt: &PhysicalAttemptId,
    ) -> Result<(Vec<RecordedAttemptUsage>, Vec<HistoryOmission>), HistoryReadError> {
        let store = |source| HistoryReadError::Store {
            session: session.as_str().to_owned(),
            source,
        };
        let mut statement = self
            .conn
            .prepare(
                "SELECT id, seq, created, type, data FROM event
                 WHERE aggregate_family = 'session' AND aggregate_id = ?1
                   AND type IN ('session.usage.observed.1', 'session.usage.observed.2')
                   AND CASE WHEN json_valid(data) = 0 THEN 1
                         WHEN json_type(data, '$.attempt') IS NOT 'text' THEN 1
                         ELSE json_extract(data, '$.attempt') = ?2 END
                 ORDER BY seq LIMIT ?3",
            )
            .map_err(store)?;
        let probe = i64::try_from(MAX_ATTEMPT_USAGE_ROWS.saturating_add(1)).unwrap_or(i64::MAX);
        let mapped = statement
            .query_map(
                rusqlite::params![session.as_str(), attempt.as_str(), probe],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .map_err(store)?;
        let mut rows = Vec::new();
        for row in mapped {
            rows.push(row.map_err(store)?);
        }
        if rows.len() > MAX_ATTEMPT_USAGE_ROWS {
            return Err(HistoryReadError::AttemptUsageTooLong {
                session: session.as_str().to_owned(),
                attempt: attempt.as_str().to_owned(),
            });
        }
        let mut history = MessageHistory::default();
        for (event, seq, created_ms, versioned, data) in rows {
            fold_usage_row(
                session,
                &mut history,
                (event, seq, created_ms, versioned, data),
            )?;
        }
        Ok((
            history.usage_for(attempt).to_vec(),
            history.omissions().to_vec(),
        ))
    }
}

/// Folds one usage row into a scratch history.
///
/// A row that cannot be read is recorded as an omission and skipped: an
/// unmeasured count stays absent and names its row, so one bad usage row costs
/// that observation and never the attempt's other measurements. A malformed
/// KIND, version or negative sequence is still corruption, because it cannot be
/// located at all.
fn fold_usage_row(
    session: &SessionId,
    history: &mut MessageHistory,
    (event, seq, created_ms, versioned, data): (String, i64, i64, String, String),
) -> Result<(), HistoryReadError> {
    let corrupt = |reason: String| HistoryReadError::Corrupt {
        session: session.as_str().to_owned(),
        seq,
        kind: versioned.clone(),
        reason,
    };
    let Some((name, version)) = versioned.rsplit_once('.') else {
        return Err(corrupt("usage kind has no version suffix".to_owned()));
    };
    let version = version
        .parse::<u32>()
        .map_err(|_| corrupt("usage kind has no numeric version".to_owned()))?;
    let seq = u64::try_from(seq).map_err(|_| corrupt("usage sequence is negative".to_owned()))?;
    let Some(value) = decode_usage_payload(history, name, seq, &data) else {
        return Ok(());
    };
    let Some(event) = decode_usage_event(history, name, seq, &event) else {
        return Ok(());
    };
    match history.apply_committed(
        HistoryRow {
            seq,
            kind_name: name,
            kind_version: version,
            data: &value,
        },
        Some(CommittedPosition {
            event,
            seq,
            created_ms,
        }),
    ) {
        Err(error) if !matches!(error, HistoryError::Payload { .. }) => {
            Err(corrupt(error.to_string()))
        }
        _ => Ok(()),
    }
}

fn decode_usage_payload(
    history: &mut MessageHistory,
    name: &str,
    seq: u64,
    data: &str,
) -> Option<Value> {
    match serde_json::from_str::<Value>(data) {
        Ok(value) => Some(value),
        Err(_) => {
            history.omit(HistoryOmission {
                seq,
                kind: name.to_owned(),
                class: HistoryOmissionClass::Undecodable,
                detail: "usage data is not JSON".to_owned(),
                message: None,
            });
            None
        }
    }
}

fn decode_usage_event(
    history: &mut MessageHistory,
    name: &str,
    seq: u64,
    event: &str,
) -> Option<EventId> {
    match event.parse::<EventId>() {
        Ok(event) => Some(event),
        Err(_) => {
            history.omit(HistoryOmission {
                seq,
                kind: name.to_owned(),
                class: HistoryOmissionClass::Unclassified,
                detail: "usage event identity is invalid".to_owned(),
                message: None,
            });
            None
        }
    }
}
