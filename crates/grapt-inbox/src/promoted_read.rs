//! Bounded reads of promoted session inputs and their durable positions.
//!
//! Copyright 2025 OpenAI, Apache-2.0.

use std::collections::HashMap;

use crate::PromotedInputKind;
use grapt_store::EventStore;
use grapt_types::{EventSeq, MessageId, SessionId};

/// Caller-owned row and identity-text bounds for a promoted-input window.
#[derive(Debug, Clone, Copy)]
pub struct PromotedReadLimits {
    pub max_rows: usize,
    pub max_identity_bytes: usize,
}

/// Preflight sizes available before any promoted input text is fetched.
#[derive(Debug, Clone, Copy)]
pub struct PromotedRowLengths {
    pub identity_bytes: usize,
    pub kind_bytes: usize,
    pub text_bytes: usize,
    pub kind: PromotedInputKind,
}

/// One promoted input, in durable sequence order.
#[derive(Debug)]
pub struct PromotedRow {
    pub seq: EventSeq,
    pub kind: PromotedInputKind,
    pub text: String,
}

/// Read a promoted-input window, including negative sequences in the scan so
/// they are refused as corruption rather than silently skipped.
///
/// Every row is validated and charged against the caller's remaining byte
/// budget before any text is fetched. The caller owns serialization charges
/// and capacity classification; storage and corruption failures convert from
/// their original detail strings without changing the caller's charge errors.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub fn window_rows<E: From<String>>(
    store: &EventStore,
    session: &SessionId,
    from: EventSeq,
    until: EventSeq,
    limits: PromotedReadLimits,
    budget: &mut usize,
    mut charge: impl FnMut(&mut usize, PromotedRowLengths) -> Result<(), E>,
) -> Result<Vec<PromotedRow>, E> {
    let PromotedReadLimits {
        max_rows,
        max_identity_bytes,
    } = limits;
    let probe_rows = max_rows
        .checked_add(1)
        .ok_or_else(|| "recent promoted row probe overflows usize".to_owned())?;
    let probe_rows = i64::try_from(probe_rows)
        .map_err(|_| "recent promoted row bound exceeds SQLite range".to_owned())?;
    let identity_limit = i64::try_from(max_identity_bytes)
        .map_err(|_| "recent promoted identity byte bound exceeds SQLite range".to_owned())?;
    let visible =
        grapt_store::history_visibility::HistoryVisibility::read(store.connection(), session)
            .map_err(|error| error.to_string())?
            .sql("seq");
    let from = i64::try_from(from.value())
        .map_err(|_| "recent window start exceeds SQLite range".to_owned())?;
    let until = i64::try_from(until.value())
        .map_err(|_| "recent window head exceeds SQLite range".to_owned())?;
    let mut statement = store
        .connection()
        .prepare(&format!(
            "SELECT COALESCE(length(CAST(id AS BLOB)), -1), id, type,
                    COALESCE(length(CAST(type AS BLOB)), -1),
                    COALESCE(length(CAST(text AS BLOB)), -1), seq
             FROM session_message
             WHERE session_id = ?1 AND type IN ('user', 'synthetic')
             AND {visible} AND (seq >= ?2 OR seq < 0) AND seq < ?3
             ORDER BY seq ASC LIMIT {}",
            probe_rows
        ))
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map((session.as_str(), from, until), |row| {
            let id_len = row.get::<_, i64>(0)?;
            let kind_len = row.get::<_, i64>(3)?;
            let id = if id_len <= identity_limit {
                Some(row.get::<_, String>(1)?)
            } else {
                None
            };
            let kind = if kind_len <= identity_limit {
                Some(row.get::<_, String>(2)?)
            } else {
                None
            };
            Ok((id, kind, row.get::<_, i64>(4)?, row.get::<_, i64>(5)?))
        })
        .map_err(|error| error.to_string())?;
    let rows = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    if rows.len() > max_rows {
        return Err(format!("recent promoted inputs exceed their {max_rows} bound").into());
    }
    let mut accepted: Vec<(MessageId, PromotedInputKind, EventSeq)> =
        Vec::with_capacity(rows.len());
    for (id, kind, text_len, seq) in rows {
        let id = id.ok_or_else(|| {
            format!(
                "recent promoted message identity exceeds its {max_identity_bytes}-byte read bound"
            )
        })?;
        let kind = kind.ok_or_else(|| {
            format!("recent promoted message kind exceeds its {max_identity_bytes}-byte read bound")
        })?;
        let message = id
            .parse::<MessageId>()
            .map_err(|error| format!("recent projected message identity is invalid: {error}"))?;
        let seq = u64::try_from(seq)
            .map_err(|_| format!("recent promoted message sequence {seq} is negative"))?;
        let input_kind = match kind.as_str() {
            "user" => PromotedInputKind::User,
            "synthetic" => PromotedInputKind::Synthetic,
            _ => {
                return Err("recent promoted message kind is not user or synthetic"
                    .to_owned()
                    .into());
            }
        };
        if text_len < 0 {
            return Err(format!("recent promoted message {id} has no text").into());
        }
        charge(
            budget,
            PromotedRowLengths {
                identity_bytes: id.len(),
                kind_bytes: kind.len(),
                text_bytes: text_len as usize,
                kind: input_kind,
            },
        )?;
        accepted.push((message, input_kind, EventSeq::new(seq)));
    }
    let mut statement = store
        .connection()
        .prepare(&format!(
            "SELECT COALESCE(length(CAST(id AS BLOB)), -1), id, text
             FROM session_message
             WHERE session_id = ?1 AND type IN ('user', 'synthetic')
             AND {visible} AND (seq >= ?2 OR seq < 0) AND seq < ?3
             ORDER BY seq ASC LIMIT {}",
            probe_rows
        ))
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map((session.as_str(), from, until), |row| {
            let fetch_id_len = row.get::<_, i64>(0)?;
            let id = if fetch_id_len <= identity_limit {
                Some(row.get::<_, String>(1)?)
            } else {
                None
            };
            Ok((id, row.get::<_, Option<String>>(2)?))
        })
        .map_err(|error| error.to_string())?;
    let mut texts: HashMap<String, Option<String>> = HashMap::with_capacity(accepted.len());
    for row in rows {
        let (id, text) = row.map_err(|error| error.to_string())?;
        let id = id.ok_or_else(|| {
            format!(
                "recent promoted fetch identity exceeds its {max_identity_bytes}-byte read bound"
            )
        })?;
        texts.insert(id, text);
    }
    let mut out = Vec::with_capacity(accepted.len());
    for (id, kind, seq) in accepted {
        let text = texts
            .remove(id.as_str())
            .flatten()
            .ok_or_else(|| format!("recent promoted message {id} has no text"))?;
        out.push(PromotedRow { seq, kind, text });
    }
    Ok(out)
}

/// Select user positions newest first, independently of lifetime input count.
/// The caller supplies its row bound; negative sequences sort first so corrupt
/// positions cannot be hidden behind otherwise valid selected inputs.
pub fn newest_user_sequences(
    store: &EventStore,
    session: &SessionId,
    current: &MessageId,
    until: EventSeq,
    max_rows: usize,
) -> Result<Vec<EventSeq>, String> {
    let until =
        i64::try_from(until.value()).map_err(|_| "retained user boundary exceeds SQLite range")?;
    let visible =
        grapt_store::history_visibility::HistoryVisibility::read(store.connection(), session)
            .map_err(|error| error.to_string())?
            .sql("seq");
    let mut statement = store
        .connection()
        .prepare(&format!(
            "SELECT seq FROM session_message WHERE session_id=?1 AND {visible}
         AND type='user' AND id<>?2 AND seq<?3
         ORDER BY (seq<0) DESC, seq DESC LIMIT ?4"
        ))
        .map_err(|error| error.to_string())?;
    let limit =
        i64::try_from(max_rows).map_err(|_| "retained user row bound exceeds SQLite range")?;
    let rows = statement
        .query_map((session.as_str(), current.as_str(), until, limit), |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| error.to_string())?;
    rows.map(|row| {
        let seq = row.map_err(|error| error.to_string())?;
        u64::try_from(seq)
            .map(EventSeq::new)
            .map_err(|_| "retained original input has a negative sequence".to_owned())
    })
    .collect()
}

#[cfg(test)]
#[test]
fn rejects_unrepresentable_read_limits() -> Result<(), Box<dyn std::error::Error>> {
    let store = EventStore::open_in_memory()?;
    let session = "ses_bounds".parse()?;
    let seq = EventSeq::new(0);
    for (max_rows, max_identity_bytes) in [(usize::MAX, 1), (1, usize::MAX)] {
        let limits = PromotedReadLimits {
            max_rows,
            max_identity_bytes,
        };
        let mut budget = 0;
        let result =
            window_rows::<String>(&store, &session, seq, seq, limits, &mut budget, |_, _| {
                Ok(())
            });
        let invalid = max_rows == usize::MAX || i64::try_from(max_identity_bytes).is_err();
        assert_eq!(
            matches!(result, Err(e) if e.starts_with("recent promoted ")),
            invalid
        );
    }
    Ok(())
}
