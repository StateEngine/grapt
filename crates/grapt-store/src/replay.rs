//! Replay fold over the durable log, with append+project consistency checks
//! against the SAME database's projection tables.
//!
//! Measured semantics (frozen clone, 2026-08-08): every aggregate is a
//! session; `event_sequence(aggregate_id, seq)` records the LAST allocated
//! sequence per aggregate (== MAX(event.seq), 305/305); `session.id` joins
//! `event_sequence.aggregate_id` 1:1. Those three parities are the store's
//! core append+project law — the oracle engine maintains them transactionally,
//! so a valid fold must find them intact on any healthy database.

use crate::log::{EventLog, LogError};
use std::collections::HashMap;

/// Per-aggregate summary produced by folding the log in replay order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateSummary {
    pub first_kind: String,
    pub last_seq: u64,
    pub events: u64,
}

#[derive(Debug, Default)]
pub struct ReplayFold {
    pub aggregates: HashMap<String, AggregateSummary>,
}

impl ReplayFold {
    pub fn from_log(log: &EventLog) -> Result<Self, LogError> {
        let mut fold = ReplayFold::default();
        log.for_each_in_replay_order(|e| {
            let entry = fold
                .aggregates
                .entry(e.aggregate_id.as_str().to_owned())
                .or_insert_with(|| AggregateSummary {
                    first_kind: e.kind.name().to_owned(),
                    last_seq: e.seq.value(),
                    events: 0,
                });
            entry.last_seq = e.seq.value();
            entry.events += 1;
        })?;
        Ok(fold)
    }
}

/// Consistency verdict between the fold and the projection tables.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ConsistencyReport {
    pub aggregates: u64,
    /// Aggregates present in the log but missing from `event_sequence`,
    /// or whose recorded seq differs from the folded last seq.
    pub sequence_table_mismatches: Vec<String>,
    /// `event_sequence` rows with no events in the log.
    pub orphan_sequence_rows: Vec<String>,
    /// Session-table rows without a log aggregate, and vice versa.
    pub session_table_mismatches: Vec<String>,
    /// Aggregates whose first surviving event is not `session.created`
    /// (measured fact — head-compaction could legally cause this).
    pub aggregates_not_starting_with_created: u64,
}

impl ConsistencyReport {
    pub fn append_project_law_holds(&self) -> bool {
        self.sequence_table_mismatches.is_empty()
            && self.orphan_sequence_rows.is_empty()
            && self.session_table_mismatches.is_empty()
    }
}

pub fn verify_against_projections(
    fold: &ReplayFold,
    conn: &rusqlite::Connection,
) -> Result<ConsistencyReport, LogError> {
    let mut report = ConsistencyReport {
        aggregates: fold.aggregates.len() as u64,
        ..ConsistencyReport::default()
    };

    // event_sequence parity: same keys, and recorded seq == folded last seq.
    let mut recorded: HashMap<String, u64> = HashMap::new();
    let mut stmt = conn.prepare("SELECT aggregate_id, seq FROM event_sequence")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let seq: i64 = row.get(1)?;
        recorded.insert(id, u64::try_from(seq).unwrap_or(0));
    }
    for (id, summary) in &fold.aggregates {
        match recorded.get(id) {
            None => report
                .sequence_table_mismatches
                .push(format!("{id}: in log, missing from event_sequence")),
            Some(&seq) if seq != summary.last_seq => {
                report.sequence_table_mismatches.push(format!(
                    "{id}: event_sequence says {seq}, log folds to {}",
                    summary.last_seq
                ))
            }
            Some(_) => {}
        }
        if summary.first_kind != "session.created" {
            report.aggregates_not_starting_with_created += 1;
        }
    }
    for id in recorded.keys() {
        if !fold.aggregates.contains_key(id) {
            report.orphan_sequence_rows.push(id.clone());
        }
    }

    // session-table parity, both directions.
    let mut session_ids: Vec<String> = Vec::new();
    let mut stmt = conn.prepare("SELECT id FROM session")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        session_ids.push(row.get(0)?);
    }
    for id in &session_ids {
        if !fold.aggregates.contains_key(id) {
            report
                .session_table_mismatches
                .push(format!("{id}: session row without log events"));
        }
    }
    for id in fold.aggregates.keys() {
        if !session_ids.iter().any(|s| s == id) {
            report
                .session_table_mismatches
                .push(format!("{id}: log aggregate without session row"));
        }
    }

    Ok(report)
}
