//! Sealed agent mail prepared by the inbox and captured with a send permit.

use crate::{InboxEvent, OperationDeliverySettlement};
use grapt_store::operation_mail::render_quoted_input as render;
use grapt_store::AggregateId;
use grapt_store::{
    AppendInput, AttemptRecordCeiling, EventStore, FencedWriter, GovernedTransaction,
};
use grapt_types::{
    DeliveryClaimId, EventId, EventSeq, EventVersion, MessageId, PhysicalAttemptId,
    RuntimeGeneration, SessionId, StepNumber, SubagentOperationId,
};
use serde_json::Value;

mod message;
mod priority;

const MAX_BYTES: usize = 1024 * 1024;
const MAX_SOURCE_BYTES: usize = 1024 * 1024;
pub const PRESENTED_KIND: &str = "session.subagent-mail.presented";
/// The presented agent-mail rows one continuation window reads. The product's
/// structural probe mirrors it, so a window past it compacts before this read
/// would refuse (SC-COMPACT-01).
pub const MAX_PRESENTED_ROWS: usize = 1_280;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationMailReceipt {
    pub operation: SubagentOperationId,
    pub claim: DeliveryClaimId,
    pub generation: RuntimeGeneration,
}

#[derive(Clone)]
struct Item {
    input: MessageId,
    seq: u64,
    data: Value,
    receipt: Option<OperationMailReceipt>,
}

/// Its content and coordinates can only be obtained from committed inbox facts.
#[derive(Clone)]
pub struct OperationMailBatch {
    session: SessionId,
    step: StepNumber,
    items: Vec<Item>,
    content: Vec<String>,
    priority: Option<MessageId>,
}

impl std::fmt::Debug for OperationMailBatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OperationMailBatch")
            .field("session", &self.session)
            .field("step", &self.step)
            .field("messages", &self.items.len())
            .finish_non_exhaustive()
    }
}

fn capture_id(input: &MessageId) -> Result<EventId, String> {
    format!("evt_agent_mail_{}", input.as_str())
        .parse()
        .map_err(|error| format!("agent mail capture identity: {error}"))
}

fn decode_item(session: &SessionId, seq: u64, data: Value) -> Result<Item, String> {
    if data.pointer("/input/type").and_then(Value::as_str) == Some("agent-message") {
        let message = grapt_store::agent_message::AgentMessage::decode(&data)?;
        if message.session != *session {
            return Err("agent message belongs to another Session".into());
        }
        return Ok(Item {
            input: message.input,
            seq,
            data,
            receipt: None,
        });
    }
    let Some(InboxEvent::OperationDeliveryAdmitted {
        session: owner,
        input,
        subagent,
        claim,
        generation,
        settlement,
        artifact,
        ..
    }) = InboxEvent::decode("session.input.admitted", 1, &data)
        .map_err(|error| error.to_string())?
    else {
        return Err("agent mail source is not an operation delivery".into());
    };
    if owner != *session
        || (settlement == OperationDeliverySettlement::Completed && artifact.is_none())
    {
        return Err("agent mail source has foreign or incomplete coordinates".into());
    }
    Ok(Item {
        input,
        seq,
        data,
        receipt: Some(OperationMailReceipt {
            operation: subagent,
            claim,
            generation,
        }),
    })
}

fn pending_results(
    conn: &rusqlite::Connection,
    session: &SessionId,
    captured: bool,
) -> Result<Vec<Item>, String> {
    let mut query = conn
        .prepare(
            "SELECT p.admitted_seq,
         CASE WHEN length(CAST(e.data AS BLOB))<=1048576 THEN e.data END,
         CASE WHEN length(CAST(p.id AS BLOB))<=512 THEN p.id END,
         CASE WHEN length(CAST(p.claim_id AS BLOB))<=512 THEN p.claim_id END,
         p.generation
         FROM session_operation_delivery_pending p LEFT JOIN event e
           ON e.aggregate_family='session' AND e.aggregate_id=p.session_id
           AND e.seq=p.admitted_seq AND e.type='session.input.admitted.1'
         WHERE p.session_id=?1 AND EXISTS(
           SELECT 1 FROM event c WHERE c.id='evt_agent_mail_' || p.id
         )=?2 ORDER BY p.admitted_seq LIMIT 8",
        )
        .map_err(|error| error.to_string())?;
    let mut rows = query
        .query((session.as_str(), captured))
        .map_err(|error| error.to_string())?;
    let mut items = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = rows.next().map_err(|error| error.to_string())? {
        let seq = u64::try_from(row.get::<_, i64>(0).map_err(|error| error.to_string())?)
            .map_err(|_| "negative operation mail sequence")?;
        let raw: Option<String> = row.get(1).map_err(|error| error.to_string())?;
        let raw = raw.ok_or("agent mail source exceeds its read bound")?;
        let data = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
        let item = decode_item(session, seq, data)?;
        let input: Option<String> = row.get(2).map_err(|error| error.to_string())?;
        let claim: Option<String> = row.get(3).map_err(|error| error.to_string())?;
        let generation = row.get::<_, i64>(4).map_err(|error| error.to_string())?;
        let receipt = item
            .receipt
            .as_ref()
            .ok_or("operation delivery projection points to arbitrary mail")?;
        if input.as_deref() != Some(item.input.as_str())
            || claim.as_deref() != Some(receipt.claim.as_str())
            || u64::try_from(generation).ok() != Some(receipt.generation.value())
        {
            return Err("pending agent mail differs from its canonical admission".into());
        }
        let size = render(&item.data)?.len();
        if bytes.saturating_add(size) > MAX_BYTES {
            if items.is_empty() {
                return Err("one agent result exceeds the presentation byte bound".into());
            }
            break;
        }
        bytes += size;
        items.push(item);
    }
    Ok(items)
}

fn pending(
    conn: &rusqlite::Connection,
    session: &SessionId,
    captured: bool,
) -> Result<Vec<Item>, String> {
    let mut items = pending_results(conn, session, captured)?;
    if captured {
        return Ok(items);
    }
    items.extend(message::pending(conn, session)?);
    items.sort_by_key(|item| item.seq);
    let mut bytes = 0usize;
    let mut kept = Vec::new();
    for item in items.into_iter().take(8) {
        let size = render(&item.data)?.len();
        if bytes.saturating_add(size) > MAX_BYTES {
            if kept.is_empty() {
                return Err("one agent message exceeds the presentation byte bound".into());
            }
            break;
        }
        bytes += size;
        kept.push(item);
    }
    Ok(kept)
}

impl OperationMailBatch {
    /// Pending operation deliveries retain their existing publication pacing;
    /// unrelated messages must not hold back a newly completed child result.
    pub fn has_unpresented_results(
        store: &EventStore,
        session: &SessionId,
    ) -> Result<bool, String> {
        Ok(!pending_results(store.connection(), session, false)?.is_empty())
    }

    pub fn has_unpresented(store: &EventStore, session: &SessionId) -> Result<bool, String> {
        Ok(!pending(store.connection(), session, false)?.is_empty())
    }

    pub fn prepare(
        store: &EventStore,
        session: &SessionId,
        step: StepNumber,
    ) -> Result<Option<Self>, String> {
        Self::prepare_with_priority(store, session, step, None)
    }

    /// Include the bound cause of a newly claimed followup even when older
    /// queue-only mail fills the normal page. Capture retains the same permit.
    pub fn prepare_with_priority(
        store: &EventStore,
        session: &SessionId,
        step: StepNumber,
        priority: Option<&MessageId>,
    ) -> Result<Option<Self>, String> {
        let items = priority::select(store.connection(), session, step, priority)?;
        if items.is_empty() {
            return Ok(None);
        }
        let content = items
            .iter()
            .map(|item| render(&item.data))
            .collect::<Result<_, _>>()?;
        Ok(Some(Self {
            session: session.clone(),
            step,
            items,
            content,
            priority: priority.cloned(),
        }))
    }

    pub fn contents(&self) -> &[String] {
        &self.content
    }

    /// A failed reservation rolls these facts back with the send permit.
    pub fn capture_within(
        &self,
        tx: &GovernedTransaction<'_>,
        attempt: &PhysicalAttemptId,
        step: StepNumber,
        writer: FencedWriter,
        ceiling: AttemptRecordCeiling,
        created_ms: i64,
    ) -> Result<(), String> {
        if step != self.step {
            return Err("agent mail batch belongs to another Step".into());
        }
        let current = priority::select(tx, &self.session, self.step, self.priority.as_ref())?;
        if current.len() < self.items.len()
            || current
                .iter()
                .zip(&self.items)
                .any(|(current, item)| current.seq != item.seq || current.data != item.data)
        {
            return Err("agent mail changed after request preparation".into());
        }
        for item in &self.items {
            let data = grapt_store::operation_mail::OperationMailPresented {
                session: self.session.clone(),
                input: item.input.clone(),
                source_seq: EventSeq::new(item.seq),
                attempt: attempt.clone(),
                step,
            }
            .encode();
            let id = capture_id(&item.input)?;
            EventStore::append_operation_mail_presentation_within(
                tx,
                AppendInput {
                    id: &id,
                    aggregate: AggregateId::Session(self.session.clone()),
                    writer,
                    kind_name: PRESENTED_KIND,
                    version: EventVersion::new(1).ok_or("mail event version")?,
                    created_ms,
                    data: &data,
                },
                ceiling,
            )
            .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    /// Acknowledgement is safe only after the exact fact entered replayable context.
    pub fn captured_receipts(
        store: &EventStore,
        session: &SessionId,
    ) -> Result<Vec<(OperationMailReceipt, i64)>, String> {
        let mut result = Vec::new();
        for item in pending(store.connection(), session, true)? {
            let id = capture_id(&item.input)?;
            let (raw, created_ms): (Option<String>, i64) = store
                .connection()
                .query_row(
                    "SELECT CASE WHEN length(CAST(data AS BLOB))<=12288 THEN data END, created
                 FROM event WHERE id=?1 AND aggregate_family='session'
                 AND aggregate_id=?2 AND type='session.subagent-mail.presented.1'",
                    (id.as_str(), session.as_str()),
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|error| error.to_string())?;
            let value: Value =
                serde_json::from_str(&raw.ok_or("captured agent mail exceeds its bound")?)
                    .map_err(|error| error.to_string())?;
            let capture = grapt_store::operation_mail::OperationMailPresented::decode(&value)?;
            if capture.source_seq.value() != item.seq
                || capture.input != item.input
                || capture.session != *session
            {
                return Err("captured agent mail differs from its canonical admission".into());
            }
            if created_ms < 0 {
                return Err("captured agent mail has negative time".into());
            }
            result.push((
                item.receipt
                    .ok_or("result acknowledgement has no operation receipt")?,
                created_ms,
            ));
        }
        Ok(result)
    }
}

/// Why captured mail could not be presented (SC-REL-07a). The two bounds are
/// capacity a compaction can answer; every other refusal is `Failed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresentedRowsError {
    /// More captures than one request presents.
    Rows {
        limit: usize,
    },
    /// The shared source-byte budget cannot cover the next source body.
    SourceBytes {
        charge: usize,
        remaining: usize,
    },
    Failed(String),
}

impl std::fmt::Display for PresentedRowsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Rows { .. } | Self::SourceBytes { .. } => {
                "conversation exceeds bounded request context; compaction required"
            }
            Self::Failed(detail) => detail,
        })
    }
}

impl std::error::Error for PresentedRowsError {}

impl From<String> for PresentedRowsError {
    fn from(detail: String) -> Self {
        Self::Failed(detail)
    }
}

impl From<&str> for PresentedRowsError {
    fn from(detail: &str) -> Self {
        Self::Failed(detail.to_owned())
    }
}

/// The same stable projection on restart, within the selected conversation cut.
/// A shared budget charges each source body before fetching it.
pub fn presented_rows(
    store: &EventStore,
    session: &SessionId,
    from: Option<EventSeq>,
    until: Option<EventSeq>,
    ceiling: u64,
    budget: Option<&mut usize>,
) -> Result<Vec<(u64, String)>, String> {
    presented_rows_classified(store, session, from, until, ceiling, budget)
        .map_err(|error| error.to_string())
}

/// [`presented_rows`], with its capacity refusals told apart from failures.
pub fn presented_rows_classified(
    store: &EventStore,
    session: &SessionId,
    from: Option<EventSeq>,
    until: Option<EventSeq>,
    ceiling: u64,
    budget: Option<&mut usize>,
) -> Result<Vec<(u64, String)>, PresentedRowsError> {
    let mut default_budget = 4 * MAX_SOURCE_BYTES;
    let budget = budget.unwrap_or(&mut default_budget);
    let visible =
        grapt_store::history_visibility::HistoryVisibility::read(store.connection(), session)
            .map_err(|error| error.to_string())?
            .sql("seq");
    let mut query = store
        .connection()
        .prepare(&format!(
            "SELECT seq, CASE WHEN length(CAST(data AS BLOB))<=12288 THEN data END,
         CASE WHEN length(type)<=256 THEN type END FROM event
         WHERE aggregate_family='session' AND aggregate_id=?1
         AND type LIKE 'session.subagent-mail.presented.%' AND {visible}
         AND seq>=?2 AND seq<?3 AND seq<?4 ORDER BY seq LIMIT {}",
            MAX_PRESENTED_ROWS + 1
        ))
        .map_err(|error| error.to_string())?;
    let bound = |value: u64| i64::try_from(value).unwrap_or(i64::MAX);
    let mut cursor = query
        .query((
            session.as_str(),
            from.map_or(0, |v| bound(v.value())),
            until.map_or(i64::MAX, |v| bound(v.value())),
            bound(ceiling),
        ))
        .map_err(|error| error.to_string())?;
    let mut result = Vec::new();
    while let Some(row) = cursor.next().map_err(|error| error.to_string())? {
        if result.len() == MAX_PRESENTED_ROWS {
            return Err(PresentedRowsError::Rows {
                limit: MAX_PRESENTED_ROWS,
            });
        }
        let seq = u64::try_from(row.get::<_, i64>(0).map_err(|error| error.to_string())?)
            .map_err(|_| "negative captured mail sequence")?;
        let kind: Option<String> = row.get(2).map_err(|error| error.to_string())?;
        if kind.as_deref() != Some("session.subagent-mail.presented.1") {
            return Err("unknown agent mail capture version".into());
        }
        let raw: Option<String> = row.get(1).map_err(|error| error.to_string())?;
        let value: Value =
            serde_json::from_str(&raw.ok_or("agent mail capture exceeds its read bound")?)
                .map_err(|error| error.to_string())?;
        let capture = grapt_store::operation_mail::OperationMailPresented::decode(&value)?;
        let source = capture.source_seq.value();
        if capture.session != *session || source >= seq {
            return Err("mail capture does not follow its own Session source".into());
        }
        let source_seq = i64::try_from(source).map_err(|_| "mail source exceeds SQLite range")?;
        let source_bytes: i64 = store
            .connection()
            .query_row(
                "SELECT length(CAST(data AS BLOB)) FROM event WHERE aggregate_family='session'
             AND aggregate_id=?1 AND seq=?2 AND type='session.input.admitted.1'",
                (session.as_str(), source_seq),
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        let charge = usize::try_from(source_bytes)
            .ok()
            .filter(|size| *size <= MAX_SOURCE_BYTES)
            .and_then(|size| size.checked_add(128))
            .ok_or("captured mail source exceeds its bound")?;
        *budget = budget
            .checked_sub(charge)
            .ok_or(PresentedRowsError::SourceBytes {
                charge,
                remaining: *budget,
            })?;
        let raw: Option<String> = store.connection().query_row(
            "SELECT CASE WHEN length(CAST(data AS BLOB))<=1048576 THEN data END FROM event
             WHERE aggregate_family='session' AND aggregate_id=?1 AND seq=?2 AND type='session.input.admitted.1'",
            (session.as_str(), source_seq), |row| row.get(0),
        ).map_err(|error| error.to_string())?;
        let delivery: Value =
            serde_json::from_str(&raw.ok_or("captured mail source exceeds its bound")?)
                .map_err(|error| error.to_string())?;
        let item = decode_item(session, source, delivery)?;
        if item.input != capture.input {
            return Err("captured mail input differs from its source".into());
        }
        let text = render(&item.data)?;
        result.push((seq, text));
    }
    Ok(result)
}

/// Whether a new result is itself progress beyond the previous compaction.
pub fn has_presented_since(
    store: &EventStore,
    session: &SessionId,
    head: EventSeq,
) -> Result<bool, String> {
    let head =
        i64::try_from(head.value()).map_err(|_| "agent mail sequence exceeds SQLite range")?;
    store
        .connection()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM event WHERE aggregate_family='session' AND aggregate_id=?1
         AND type LIKE 'session.subagent-mail.presented.%' AND seq>?2)",
            (session.as_str(), head),
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())
}
