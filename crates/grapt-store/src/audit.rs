//! Read-only audit: the Rust engine's first contact with real data. Decodes
//! EVERY event strictly and measures the replay preconditions.
//!
//! EMPIRICAL LAW (measured on the frozen clone, 2026-08-08, this audit):
//! within an aggregate the durable sequence is zero-based and STRICTLY
//! INCREASING but NOT contiguous — real aggregates carry forward gaps
//! (oracle-era deletion/compaction leaves holes; e.g. `ses_04b1327e9ffe…` jumps
//! 1053→1075). The sound replay precondition is monotonicity; a fold that
//! demands contiguity rejects the real corpus. Gap statistics are measured
//! facts the exporter and migration must carry, not defects.

use crate::log::{EventLog, LogError};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct AuditReport {
    pub events: u64,
    pub aggregates: u64,
    pub distinct_kinds: u64,
    /// Replay-breaking: a sequence that failed to strictly increase
    /// (duplicate or backward) within its aggregate.
    pub monotonicity_violations: Vec<String>,
    /// Measured facts, not defects:
    pub aggregates_with_gaps: u64,
    pub missing_sequence_numbers: u64,
    pub aggregates_starting_nonzero: u64,
    /// Events whose data contains an explicit JSON null anywhere.
    pub explicit_null_events: u64,
}

impl AuditReport {
    pub fn replay_preconditions_hold(&self) -> bool {
        self.monotonicity_violations.is_empty()
    }
}

fn contains_null(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Array(items) => items.iter().any(contains_null),
        Value::Object(map) => map.values().any(contains_null),
        _ => false,
    }
}

pub fn audit(path: &Path) -> Result<AuditReport, LogError> {
    let log = EventLog::open_read_only(path)?;
    // aggregate -> (last_seq, has_gap)
    let mut state: HashMap<String, (u64, bool)> = HashMap::new();
    let mut kinds: HashMap<String, u64> = HashMap::new();
    let mut report = AuditReport::default();

    report.events = log.for_each_in_replay_order(|e| {
        let seq = e.seq.value();
        match state.get_mut(e.aggregate_id.as_str()) {
            None => {
                if seq != 0 {
                    report.aggregates_starting_nonzero += 1;
                    report.missing_sequence_numbers += seq;
                }
                state.insert(e.aggregate_id.as_str().to_owned(), (seq, seq != 0));
            }
            Some((last, has_gap)) => {
                if seq <= *last {
                    report.monotonicity_violations.push(format!(
                        "{}: seq {} after {}",
                        e.aggregate_id.as_str(),
                        seq,
                        last
                    ));
                } else if seq > *last + 1 {
                    report.missing_sequence_numbers += seq - *last - 1;
                    *has_gap = true;
                }
                *last = seq;
            }
        }
        *kinds
            .entry(format!("{}.{}", e.kind.name(), e.kind.version().value()))
            .or_insert(0) += 1;
        if contains_null(&e.data) {
            report.explicit_null_events += 1;
        }
    })?;

    report.aggregates = state.len() as u64;
    report.aggregates_with_gaps = state.values().filter(|(_, gap)| *gap).count() as u64;
    report.distinct_kinds = kinds.len() as u64;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_detection_walks_nested_structures() -> Result<(), serde_json::Error> {
        let v: Value = serde_json::from_str("{\"a\":[1,{\"b\":null}]}")?;
        assert!(contains_null(&v));
        let clean: Value = serde_json::from_str("{\"a\":[1,{\"b\":2}]}")?;
        assert!(!contains_null(&clean));
        Ok(())
    }
}
