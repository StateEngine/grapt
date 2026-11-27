//! Bounded accounting view owned by Session history, not by a product SQL copy.
use super::*;
use std::collections::HashSet;
mod compaction;
mod dispatch;
mod inherited;
mod request_cache;
pub use request_cache::AccountingRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountingOperation {
    Assistant,
    Compaction,
    Unknown,
}

// A producer day can exceed 256 calls. Keep the complete read bounded, with at
// most 65 keyset pages inside Store's unchanged 128-statement snapshot budget.
pub const MAX_ACCOUNTING_ROWS: usize = 65_536;
pub const MAX_ACCOUNTING_ATTEMPTS: usize = 8192;
pub const MAX_ACCOUNTING_BYTES: usize = 64 * 1024 * 1024;
const PAGE_ROWS: usize = 1024;
const ROW_BYTES: usize = 65536;

#[derive(Debug)]
pub struct AccountingAttempt {
    pub attempt: PhysicalAttemptId,
    pub request_cache: Option<AccountingRequest>,
    pub operation: AccountingOperation,
    pub send_permitted: bool,
    pub terminal_observed: bool,
    pub step: Option<StepNumber>,
    pub selection: Option<session_message::AssistantSelection>,
    pub usage: grapt_types::ReconciledUsage,
    pub opening: Option<CommittedPosition>,
    pub first_block: Option<CommittedPosition>,
    pub step_terminal: Option<StepTerminal>,
    pub completed_at: Option<grapt_event::session_step_observed::CompletionObservedAt>,
}

#[derive(Debug)]
pub struct AccountingSnapshot {
    pub session: SessionId,
    pub last_seq: Option<u64>,
    pub events_read: usize,
    pub bytes_read: usize,
    pub enumeration_complete: bool,
    pub operational_coverage_complete: bool,
    pub attempts: Vec<AccountingAttempt>,
}

impl SessionHistoryStore<'_> {
    /// The store-owned capability holds one read transaction across all keyset pages.
    /// No cursor escapes its snapshot; an interrupted read restarts from scratch.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn accounting_snapshot(
        source: &grapt_store::read_snapshot::ReadSnapshot<'_>,
        session: &SessionId,
    ) -> Result<AccountingSnapshot, String> {
        if session.as_str().len() > 128 {
            return Err("accounting session identity exceeds 128 bytes".into());
        }
        let ledger = grapt_store::provider_attempt_accounting::read(
            source,
            session,
            grapt_store::AttemptRecordCeiling::new(
                std::num::NonZeroU32::new(65536).ok_or("accounting record bound")?,
            ),
            MAX_ACCOUNTING_ATTEMPTS,
        )?;
        let mut history = MessageHistory::default();
        let mut blocks: HashMap<PhysicalAttemptId, CommittedPosition> = HashMap::new();
        let mut compactions = compaction::Calls::default();
        let mut requests = request_cache::Requests::default();
        let mut snapshot = AccountingSnapshot {
            session: session.clone(),
            last_seq: None,
            events_read: 0,
            bytes_read: ledger.bytes_read,
            enumeration_complete: false,
            operational_coverage_complete: false,
            attempts: Vec::new(),
        };
        let parent = inherited::read(source, session, &mut snapshot)?;
        let mut after: Option<i64> = None;
        loop {
            // The first page must include every sequence, including corrupt
            // negative values. Later pages need a range seek, not a nullable
            // predicate that rescans the prefix and spends Store's VM budget.
            let query = if after.is_some() {
                "SELECT seq, created,
                    CASE WHEN length(CAST(id AS BLOB)) <= 128 THEN id END,
                    CASE WHEN length(CAST(type AS BLOB)) <= 128 THEN type END,
                    CASE WHEN length(CAST(data AS BLOB)) <= ?4 THEN data END
                 FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
                 AND seq > ?2 ORDER BY seq LIMIT ?3"
            } else {
                "SELECT seq, created,
                    CASE WHEN length(CAST(id AS BLOB)) <= 128 THEN id END,
                    CASE WHEN length(CAST(type AS BLOB)) <= 128 THEN type END,
                    CASE WHEN length(CAST(data AS BLOB)) <= ?4 THEN data END
                 FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
                 ORDER BY seq LIMIT ?3"
            };
            let mut statement = source
                .prepare(query)
                .map_err(|e| format!("accounting snapshot query failed: {e}"))?;
            let mut rows = statement
                .query(rusqlite::params![
                    session.as_str(),
                    after,
                    PAGE_ROWS as i64,
                    ROW_BYTES as i64
                ])
                .map_err(|e| format!("accounting snapshot read failed: {e}"))?;
            let mut page_rows = 0;
            while let Some(row) = rows
                .next()
                .map_err(|e| format!("accounting row read failed: {e}"))?
            {
                if snapshot.events_read == MAX_ACCOUNTING_ROWS {
                    return finish(
                        history,
                        blocks,
                        compactions,
                        requests,
                        snapshot,
                        ledger,
                        &parent,
                    );
                }
                let seq: i64 = row
                    .get(0)
                    .map_err(|_| "accounting sequence is not an integer")?;
                let created: i64 = row
                    .get(1)
                    .map_err(|_| "accounting timestamp is not an integer")?;
                let event: Option<String> = row
                    .get(2)
                    .map_err(|_| "accounting event identity is unreadable")?;
                let kind: Option<String> = row
                    .get(3)
                    .map_err(|_| "accounting event kind is unreadable")?;
                let data: Option<String> = row
                    .get(4)
                    .map_err(|_| "accounting event data is unreadable")?;
                let (Some(event), Some(kind), Some(data)) = (event, kind, data) else {
                    return Err(row_error(
                        seq,
                        "unbounded row",
                        "identity, kind or data exceeds its byte bound or is null",
                    ));
                };
                snapshot.bytes_read = snapshot
                    .bytes_read
                    .checked_add(event.len() + kind.len() + data.len())
                    .ok_or("accounting input byte overflow")?;
                if snapshot.bytes_read > MAX_ACCOUNTING_BYTES {
                    return Err("accounting input exceeds 64 MiB".into());
                }
                if after.is_some_and(|held| seq <= held) {
                    return Err(row_error(seq, &kind, "the sequence did not advance"));
                }
                snapshot.events_read += 1;
                if let Ok(seq) = u64::try_from(seq) {
                    snapshot.last_seq = Some(seq);
                }
                after = Some(seq);
                page_rows += 1;
                if request_cache::owns(&kind) {
                    if seq < 0 {
                        return Err(row_error(seq, &kind, "negative sequence"));
                    }
                    requests
                        .apply(&kind, &data, created)
                        .map_err(|e| row_error(seq, &kind, e))?;
                    continue;
                }
                if compaction::owns(&kind) {
                    let position = CommittedPosition {
                        event: event
                            .parse::<EventId>()
                            .map_err(|_| row_error(seq, &kind, "invalid event identity"))?,
                        seq: u64::try_from(seq)
                            .map_err(|_| row_error(seq, &kind, "negative sequence"))?,
                        created_ms: created,
                    };
                    let attempt = compactions
                        .apply(&kind, &data, position)
                        .map_err(|e| row_error(seq, &kind, e))?;
                    requests.note_attempt(&attempt);
                    check_attempt_bound(&history, compactions.len())?;
                    continue;
                }
                // The owner decides what is read, skipped or corrupt; rows it
                // skips (other eras, unowned kinds) are skipped here too.
                let Some(row) = super::store::classify_row(&kind, seq, &data)
                    .map_err(|(kind, reason)| row_error(seq, &kind, reason))?
                else {
                    continue;
                };
                let event = event.parse::<EventId>().map_err(|_| {
                    row_error(seq, row.name, "the stored event identity is invalid")
                })?;
                let position = CommittedPosition {
                    event,
                    seq: row.seq,
                    created_ms: created,
                };
                if row.name == "session.assistant-message.opened" && matches!(row.version, 1 | 2) {
                    let fact = if row.version == 2 {
                        session_message::decode_opened_v2(&row.value)
                    } else {
                        session_message::decode_opened(&row.value)
                    }
                    .map_err(|e| row_error(seq, row.name, e))?;
                    requests.note_attempt(fact.attempt());
                }
                // Existing owner fold validates identities, versions and duplicate terminals.
                history
                    .apply_committed(
                        HistoryRow {
                            seq: row.seq,
                            kind_name: row.name,
                            kind_version: row.version,
                            data: &row.value,
                        },
                        Some(position.clone()),
                    )
                    .map_err(|e| row_error(seq, row.name, e))?;
                if row.name == "session.content-block.opened" && row.version == 1 {
                    let fact = session_message::decode_block_opened(&row.value)
                        .map_err(|e| row_error(seq, row.name, e))?;
                    requests.note_attempt(fact.attempt());
                    blocks.entry(fact.attempt().clone()).or_insert(position);
                }
                if row.name == "session.usage.observed" && matches!(row.version, 1 | 2) {
                    // The history owner has validated both usage eras above.
                    let attempt = row
                        .value
                        .get("attempt")
                        .and_then(Value::as_str)
                        .and_then(PhysicalAttemptId::new)
                        .ok_or_else(|| row_error(seq, row.name, "invalid usage attempt"))?;
                    requests.note_attempt(&attempt);
                }
                if history.usage.values().any(|values| values.len() > 64) {
                    return Err(row_error(
                        seq,
                        row.name,
                        "an attempt exceeds 64 usage observations",
                    ));
                }
                check_attempt_bound(&history, compactions.len())?;
            }
            if page_rows < PAGE_ROWS {
                snapshot.enumeration_complete = true;
                return finish(
                    history,
                    blocks,
                    compactions,
                    requests,
                    snapshot,
                    ledger,
                    &parent,
                );
            }
        }
    }
}

fn check_attempt_bound(history: &MessageHistory, compactions: usize) -> Result<(), String> {
    if compactions
        + history.attempt_messages.len()
        + history
            .usage
            .keys()
            .filter(|id| !history.attempt_messages.contains_key(*id))
            .count()
        > MAX_ACCOUNTING_ATTEMPTS
    {
        return Err(format!(
            "accounting exceeds {MAX_ACCOUNTING_ATTEMPTS} attempts"
        ));
    }
    Ok(())
}

/// Locates a refusal the way the owner's `HistoryReadError::Corrupt` does.
fn row_error(seq: i64, kind: &str, reason: impl std::fmt::Display) -> String {
    format!("accounting row at seq {seq} ({kind}): {reason}")
}

fn finish(
    history: MessageHistory,
    blocks: HashMap<PhysicalAttemptId, CommittedPosition>,
    compactions: compaction::Calls,
    requests: request_cache::Requests,
    mut snapshot: AccountingSnapshot,
    ledger: grapt_store::provider_attempt_accounting::ProviderAttemptAccounting,
    parent: &MessageHistory,
) -> Result<AccountingSnapshot, String> {
    if snapshot.events_read == 0 {
        return Err("accounting Session has no durable events".into());
    }
    let inherited = inherited::identities(parent, &history)?;
    if ledger
        .attempts
        .iter()
        .any(|record| inherited.contains(record.receipt().attempt()))
    {
        return Err("child dispatch reuses an inherited Physical Attempt".into());
    }
    let mut identities: HashSet<PhysicalAttemptId> =
        history.attempt_messages.keys().cloned().collect();
    identities.extend(history.usage.keys().cloned());
    let mut identities: Vec<_> = identities
        .into_iter()
        .filter(|id| !inherited.contains(id))
        .collect();
    identities.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    for attempt in identities {
        let opening = history
            .attempt_messages
            .get(&attempt)
            .and_then(|message| history.messages.get(message));
        let step = opening
            .map(|opened| opened.step)
            .or_else(|| requests.step(&attempt));
        // A message's initial model does not prove the model of a later retry.
        let exact_opening = opening.filter(|opened| opened.attempt == attempt);
        let usage = grapt_types::usage::reconcile_recorded_usage(history.usage_for(&attempt))
            .map_err(|e| e.to_string())?;
        snapshot.attempts.push(AccountingAttempt {
            terminal_observed: step.is_some_and(|s| history.terminals.contains_key(&s.value())),
            operation: AccountingOperation::Assistant,
            send_permitted: false,
            request_cache: None,
            selection: exact_opening.and_then(|opened| opened.selection.clone()),
            opening: exact_opening.and_then(|opened| opened.committed.clone()),
            first_block: blocks.get(&attempt).cloned(),
            step_terminal: step.and_then(|s| history.terminals.get(&s.value()).cloned()),
            completed_at: step
                .and_then(|s| history.completion_observations.get(&s.value()).copied()),
            attempt,
            step,
            usage,
        });
    }
    compactions.append_to(&mut snapshot)?;
    dispatch::merge(&mut snapshot, ledger, &history)?;
    requests.attach(&mut snapshot)?;
    snapshot
        .attempts
        .sort_by(|a, b| a.attempt.as_str().cmp(b.attempt.as_str()));
    Ok(snapshot)
}

#[cfg(test)]
mod tests;
