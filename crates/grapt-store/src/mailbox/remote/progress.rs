//! Page progress in the ordinary governed fact log. No second queue or schema.
//! A page receipt commits together with healthy admissions; recovery settles
//! their original ACK debt before exposing its protocol-specific checkpoint.
use super::{
    read_budget::RecordByteBudget, refusal, require_bound_budgeted, RemoteMailboxInput,
    RemoteMailboxScope,
};
use crate::governed::GovernedTransaction;
use crate::{AppendInput, DecodeError, EventStore, FencedWriter, WriteError};
use grapt_event::{mail_progress::*, AggregateId};
use grapt_types::remote_mail::{ack::DeliveryAdmissionAck, progress::*, *};
use grapt_types::{EventId, EventSeq, EventVersion};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug)]
pub struct DecodedMailProgress {
    pub seq: EventSeq,
    pub data: Value,
    pub fact: MailPageRecorded,
}
#[derive(Debug)]
pub struct MailProgressPage {
    pub recorded: DecodedMailProgress,
    pub has_more: bool,
    pub decoded_bytes: usize,
    pub recovery: Vec<MailProgressRecovery>,
}
#[derive(Debug)]
pub struct MailProgressRecovery {
    pub ack: DeliveryAdmissionAck,
    pub delivery_pending: bool,
    pub repair_pending: bool,
    pub actionable: bool,
}
#[derive(Debug)]
pub struct RecordMailPage<'a> {
    pub scope: &'a RemoteMailboxScope,
    pub protocol: MailTransportVersion,
    pub previous: Option<&'a ExclusiveSourceCursor>,
    pub checkpoint: &'a ExclusiveSourceCursor,
    pub messages: &'a [RemoteMailboxInput],
    pub gaps: &'a [RefusedMailRow],
    pub decode_failures: &'a [MailDecodeFailure],
    pub created_ms: i64,
}
fn stream(scope: &RemoteMailboxScope, protocol: MailTransportVersion) -> MailStream {
    MailStream {
        projects: scope.projects.clone(),
        principal: scope.principal.clone(),
        recipient: scope.recipient.clone(),
        protocol,
    }
}
pub fn stream_id(
    scope: &RemoteMailboxScope,
    protocol: MailTransportVersion,
) -> Result<MailStreamId, WriteError> {
    let mut hash = Sha256::new();
    for value in [
        "secode-mail-stream/1",
        scope.projects.source().as_str(),
        scope.projects.target().as_str(),
        scope.principal.as_str(),
        scope.recipient.as_str(),
        protocol.as_str(),
    ] {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    MailStreamId::try_new(format!("mail-{:x}", hash.finalize())).map_err(|e| refusal(e.to_string()))
}
pub(crate) fn bound_input(value: &Value) -> Result<(), DecodeError> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_MAIL_PROGRESS_BYTES.saturating_sub(self.0) {
                return Err(std::io::Error::other("mail progress byte bound"));
            }
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(0), value)
        .map_err(|_| DecodeError::MailProgress("payload byte bound".into()))
}
pub(crate) fn decode_fact(
    id: &str,
    aggregate: &str,
    seq: i64,
    _created: i64,
    kind: &str,
    data: &str,
) -> Result<DecodedMailProgress, DecodeError> {
    let bad = |reason: &str| DecodeError::MailProgress(reason.into());
    if kind != "mail.page.recorded.1" || data.len() > MAX_MAIL_PROGRESS_BYTES {
        return Err(bad("unsupported kind or byte bound"));
    }
    let _: EventId = id.parse().map_err(|_| bad("event identity"))?;
    let text = data;
    let data: Value = serde_json::from_str(text).map_err(|_| bad("payload JSON"))?;
    if grapt_event::canonical_string(&data) != text {
        return Err(bad("noncanonical mail progress"));
    }
    if id != format!("evt_{}", grapt_event::canonical_hash_hex(&data)) {
        return Err(bad(
            "mail progress event identity does not match canonical payload",
        ));
    }
    let fact = MailPageRecorded::decode(&data).map_err(DecodeError::MailProgress)?;
    let scope = RemoteMailboxScope {
        projects: fact.stream.projects.clone(),
        principal: fact.stream.principal.clone(),
        recipient: fact.stream.recipient.clone(),
    };
    if stream_id(&scope, fact.stream.protocol)
        .map_err(|_| bad("stream identity"))?
        .as_str()
        != aggregate
    {
        return Err(bad("payload receiving authority differs from aggregate"));
    }
    Ok(DecodedMailProgress {
        seq: EventSeq::new(u64::try_from(seq).map_err(|_| bad("negative sequence"))?),
        data,
        fact,
    })
}
fn validate_references(
    conn: &rusqlite::Connection,
    scope: &RemoteMailboxScope,
    fact: &MailPageRecorded,
    max_bytes: usize,
) -> Result<(Vec<MailProgressRecovery>, usize), WriteError> {
    let mut recovery = Vec::with_capacity(fact.messages().len());
    let mut budget = RecordByteBudget::new(max_bytes);
    for reference in fact.messages() {
        let record = require_bound_budgeted(conn, scope, &reference.local_message, &mut budget)?;
        if record.ack().local_admission() != reference.admission
            || record.ack().canonical_message_digest() != &reference.digest
        {
            return Err(refusal(
                "mail progress admission reference differs from durable message",
            ));
        }
        recovery.push(MailProgressRecovery {
            ack: record.ack().clone(),
            delivery_pending: record.delivery_pending(),
            repair_pending: record.repair_pending(),
            actionable: record.actionable(),
        });
    }
    Ok((recovery, budget.used()))
}
impl EventStore {
    /// Explicit election before page writes. The protocol context never adopts
    /// legacy rows or another transport's ACK debt by guessing a cursor prefix.
    pub fn prepare_remote_mail_progress(
        &mut self,
        scope: &RemoteMailboxScope,
        protocol: MailTransportVersion,
    ) -> Result<(), WriteError> {
        let id = stream_id(scope, protocol)?;
        let other = stream_id(
            scope,
            if protocol == MailTransportVersion::V1 {
                MailTransportVersion::V2
            } else {
                MailTransportVersion::V1
            },
        )?;
        let conflicting: bool = self.connection().query_row(
            "SELECT EXISTS(SELECT 1 FROM event WHERE aggregate_family='mail' AND aggregate_id=?1)",
            [other.as_str()],
            |r| r.get(0),
        )?;
        if conflicting {
            return Err(refusal("mail transport checkpoint context mismatch"));
        }
        let recorded: bool = self.connection().query_row(
            "SELECT EXISTS(SELECT 1 FROM event WHERE aggregate_family='mail' AND aggregate_id=?1)",
            [id.as_str()],
            |r| r.get(0),
        )?;
        if protocol == MailTransportVersion::V2 && !recorded {
            let legacy: bool = self.connection().query_row(
                "SELECT EXISTS(SELECT 1 FROM mailbox_message WHERE recipient=?1)",
                [scope.recipient.as_str()],
                |r| r.get(0),
            )?;
            if legacy {
                return Err(refusal(
                    "v2 requires its own recorded admission context; legacy debt is preserved",
                ));
            }
        }
        self.elect_writer(&AggregateId::Mail(id))?;
        Ok(())
    }
    /// A v1 runtime must not send v2 original ACK coordinates through v1.
    pub fn require_v1_mail_context(&self, scope: &RemoteMailboxScope) -> Result<(), WriteError> {
        let id = stream_id(scope, MailTransportVersion::V2)?;
        let found: bool = self.connection().query_row(
            "SELECT EXISTS(SELECT 1 FROM event WHERE aggregate_family='mail' AND aggregate_id=?1)",
            [id.as_str()],
            |r| r.get(0),
        )?;
        if found {
            return Err(refusal("mail transport checkpoint context mismatch"));
        }
        Ok(())
    }
    pub fn record_remote_mail_page(
        &mut self,
        input: RecordMailPage<'_>,
    ) -> Result<Vec<DeliveryAdmissionAck>, WriteError> {
        let aggregate = AggregateId::Mail(stream_id(input.scope, input.protocol)?);
        let writer = self.fencing_token(&aggregate)?;
        let tx = self.governed_transaction()?;
        let result = Self::record_remote_mail_page_within(&tx, input, writer)?;
        tx.commit()?.discard();
        Ok(result)
    }
    pub fn record_remote_mail_page_within(
        tx: &GovernedTransaction<'_>,
        input: RecordMailPage<'_>,
        writer: FencedWriter,
    ) -> Result<Vec<DeliveryAdmissionAck>, WriteError> {
        let aggregate = AggregateId::Mail(stream_id(input.scope, input.protocol)?);
        crate::election::fence(tx, &aggregate, writer.epoch())?;
        if input.protocol != MailTransportVersion::V2
            || input
                .messages
                .len()
                .saturating_add(input.gaps.len())
                .saturating_add(input.decode_failures.len())
                > 64
        {
            return Err(refusal("unsupported mail page recording context or count"));
        }
        if !input
            .messages
            .iter()
            .any(|m| &m.source_cursor == input.checkpoint)
            && !input.gaps.iter().any(|g| g.cursor() == input.checkpoint)
            && !input
                .decode_failures
                .iter()
                .any(|f| f.cursor() == input.checkpoint)
        {
            return Err(refusal(
                "mail checkpoint does not name an emitted page entry",
            ));
        }
        let id = stream_id(input.scope, input.protocol)?;
        let head = read_head(tx, input.scope, input.protocol)?;
        let duplicate = head
            .as_ref()
            .is_some_and(|h| &h.checkpoint == input.checkpoint);
        if !duplicate && head.as_ref().map(|h| &h.checkpoint) != input.previous {
            return Err(refusal("mail progress does not extend the durable prefix"));
        }
        let mut acks = Vec::with_capacity(input.messages.len());
        let mut references = Vec::with_capacity(input.messages.len());
        for message in input.messages {
            if !input.scope.same_recipient_binding(&message.scope) {
                return Err(refusal("foreign page receiving authority"));
            }
            let ack = Self::admit_remote_mailbox_within(tx, &message.scope, message)?.ack;
            references.push(RecordedMail {
                local_message: ack.local_message().clone(),
                admission: ack.local_admission(),
                digest: ack.canonical_message_digest().clone(),
                enumerated_at: message.source_cursor.clone(),
            });
            acks.push(ack);
        }
        let fact = MailPageRecorded::new_with_decode_failures(
            stream(input.scope, input.protocol),
            input.previous.cloned(),
            input.checkpoint.clone(),
            references,
            input.gaps.to_vec(),
            input.decode_failures.to_vec(),
        )
        .map_err(refusal)?;
        let data = fact.encode();
        bound_input(&data)?;
        if duplicate {
            if head.as_ref() != Some(&fact) {
                return Err(refusal(
                    "duplicate mail page conflicts with durable progress",
                ));
            }
            return Ok(acks);
        }
        let event: EventId = format!("evt_{}", grapt_event::canonical_hash_hex(&data))
            .parse()
            .map_err(|_| refusal("mail progress event identity"))?;
        Self::append_mail_progress_within(
            tx,
            AppendInput {
                id: &event,
                aggregate: AggregateId::Mail(id),
                writer,
                kind_name: "mail.page.recorded",
                version: EventVersion::new(1).ok_or_else(|| refusal("event version"))?,
                created_ms: input.created_ms,
                data: &data,
            },
        )?;
        Ok(acks)
    }
    /// One complete local page fact, never an unbounded restart census.
    pub fn remote_mail_progress_page(
        &self,
        scope: &RemoteMailboxScope,
        protocol: MailTransportVersion,
        after: Option<EventSeq>,
        max_entries: u32,
        max_bytes: usize,
    ) -> Result<Option<MailProgressPage>, WriteError> {
        if max_bytes == 0 || max_entries == 0 {
            return Err(WriteError::RemoteMailProgressBudget);
        }
        let id = stream_id(scope, protocol)?;
        let mut statement=self.connection().prepare("SELECT CASE WHEN length(CAST(id AS BLOB))<=256 THEN id END,seq,created,CASE WHEN length(CAST(type AS BLOB))<=256 THEN type END,CASE WHEN length(CAST(data AS BLOB))<=262144 THEN data END,writer_epoch FROM event WHERE aggregate_family='mail' AND aggregate_id=?1 AND seq>?2 ORDER BY seq LIMIT 2")?;
        let lower = match after {
            None => -1,
            Some(seq) => i64::try_from(seq.value())
                .map_err(|_| refusal("mail progress recovery sequence exceeds SQL bound"))?,
        };
        let mut rows = statement.query(rusqlite::params![id.as_str(), lower])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let epoch: Option<i64> = row.get(5)?;
        if epoch.is_none_or(|epoch| epoch <= 0 || epoch > i64::from(u32::MAX)) {
            return Err(refusal("mail progress has no valid writer epoch"));
        }
        let raw: Option<String> = row.get(4)?;
        let raw = raw.ok_or_else(|| refusal("mail progress exceeds durable byte bound"))?;
        if raw.len() > max_bytes {
            return Err(WriteError::RemoteMailProgressBudget);
        }
        let fact = decode_fact(
            &row.get::<_, String>(0)?,
            id.as_str(),
            row.get(1)?,
            row.get(2)?,
            &row.get::<_, String>(3)?,
            &raw,
        )?;
        if fact
            .fact
            .messages()
            .len()
            .saturating_add(fact.fact.gaps().len())
            .saturating_add(fact.fact.decode_failures().len())
            > max_entries as usize
        {
            return Err(WriteError::RemoteMailProgressBudget);
        }
        let (recovery, reference_bytes) = validate_references(
            self.connection(),
            scope,
            &fact.fact,
            max_bytes.saturating_sub(raw.len()),
        )?;
        let has_more = rows.next()?.is_some();
        Ok(Some(MailProgressPage {
            recorded: fact,
            has_more,
            decoded_bytes: raw.len() + reference_bytes,
            recovery,
        }))
    }
    pub fn remote_mail_progress_acks(
        &self,
        scope: &RemoteMailboxScope,
        fact: &MailPageRecorded,
    ) -> Result<Vec<DeliveryAdmissionAck>, WriteError> {
        if fact.stream != stream(scope, fact.stream.protocol) {
            return Err(refusal(
                "mail progress belongs to another receiving authority",
            ));
        }
        validate_references(self.connection(), scope, fact, 4_194_304)
            .map(|(rows, _)| rows.into_iter().map(|row| row.ack).collect())
    }
}
type ProgressHead = (String, i64, i64, String, Option<String>, Option<i64>);
fn read_head(
    conn: &rusqlite::Connection,
    scope: &RemoteMailboxScope,
    protocol: MailTransportVersion,
) -> Result<Option<MailPageRecorded>, WriteError> {
    use rusqlite::OptionalExtension;
    let id = stream_id(scope, protocol)?;
    let row:Option<ProgressHead>=conn.query_row("SELECT CASE WHEN length(CAST(id AS BLOB))<=256 THEN id END,seq,created,CASE WHEN length(CAST(type AS BLOB))<=256 THEN type END,CASE WHEN length(CAST(data AS BLOB))<=262144 THEN data END,writer_epoch FROM event WHERE aggregate_family='mail' AND aggregate_id=?1 ORDER BY seq DESC LIMIT 1",[id.as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
    row.map(|(event, seq, created, kind, raw, epoch)| {
        if epoch.is_none_or(|e| e <= 0 || e > i64::from(u32::MAX)) {
            return Err(refusal("mail progress has no valid writer epoch"));
        }
        let fact = decode_fact(
            &event,
            id.as_str(),
            seq,
            created,
            &kind,
            &raw.ok_or_else(|| refusal("oversized mail progress head"))?,
        )?
        .fact;
        validate_references(conn, scope, &fact, 4_194_304)?;
        Ok(fact)
    })
    .transpose()
}

#[cfg(test)]
mod aux_tests;
