//! Durable request diagnostics fold in admission order inside the same read snapshot.
use super::*;
use grapt_event::request_cache::{self, RequestCacheFact};

#[derive(Debug)]
pub struct AccountingRequest {
    pub fact: RequestCacheFact,
    pub admitted_ms: i64,
    pub previous_attempt: Option<PhysicalAttemptId>,
    pub comparison: Option<grapt_types::request_cache::RequestCacheComparison>,
}
#[derive(Default)]
pub(super) struct Requests {
    records: HashMap<PhysicalAttemptId, AccountingRequest>,
    last: Option<PhysicalAttemptId>,
}
pub(super) fn owns(kind: &str) -> bool {
    kind == request_cache::KIND
        || kind
            .strip_prefix(request_cache::KIND)
            .is_some_and(|suffix| suffix.starts_with('.'))
}
impl Requests {
    pub(super) fn step(&self, attempt: &PhysicalAttemptId) -> Option<StepNumber> {
        self.records.get(attempt).map(|request| request.fact.step)
    }

    pub(super) fn note_attempt(&mut self, attempt: &PhysicalAttemptId) {
        // A legacy or otherwise unrecorded request breaks adjacency. Never compare across it.
        if !self.records.contains_key(attempt) {
            self.last = None;
        }
    }

    pub(super) fn apply(&mut self, kind: &str, raw: &str, at: i64) -> Result<(), String> {
        if kind != "session.request.cache.1" {
            return Err("unsupported request cache version".into());
        }
        if raw.len() > request_cache::MAX_RECORD_BYTES {
            return Err("request cache record byte bound".into());
        }
        let fact = request_cache::decode(
            &serde_json::from_str::<Value>(raw).map_err(|_| "request cache payload is not JSON")?,
        )?;
        if self.records.len() == MAX_ACCOUNTING_ATTEMPTS {
            return Err("request cache attempt bound".into());
        }
        if self.records.contains_key(&fact.attempt) {
            return Err("duplicate request cache attempt".into());
        }
        let comparison = self
            .last
            .as_ref()
            .and_then(|id| self.records.get(id))
            .and_then(|previous| previous.fact.fingerprint.as_ref())
            .zip(fact.fingerprint.as_ref())
            .map(|(previous, current)| current.compare(previous));
        let attempt = fact.attempt.clone();
        self.records.insert(
            attempt.clone(),
            AccountingRequest {
                comparison,
                fact,
                admitted_ms: at,
                previous_attempt: self.last.replace(attempt),
            },
        );
        Ok(())
    }
    pub(super) fn attach(mut self, snapshot: &mut AccountingSnapshot) -> Result<(), String> {
        for attempt in &mut snapshot.attempts {
            if let Some(request) = self.records.remove(&attempt.attempt) {
                if attempt.step.is_some_and(|step| step != request.fact.step) {
                    return Err("request cache Step conflicts with accounting".into());
                }
                attempt.request_cache = Some(request);
            }
        }
        Ok(())
    }
}
