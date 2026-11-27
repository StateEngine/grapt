//! Remote receipt metadata in the existing governed mailbox record.
//!
//! The body exists once, as its existing canonical record. Delivery coordinates
//! survive reconnect; current attention/disposition never rewrites identity.
use super::record::{decode_record, encode_record, push_field, Reader};
use super::{decode_row, decode_row_checked, next_seq, StoredRow};
use crate::governed::{GovernedTable, GovernedTransaction};
use crate::{EventStore, MailboxAdmitted, WriteError, MAX_MAILBOX_PAGE_MESSAGES};
use grapt_types::remote_mail::ack::{DeliveryAdmissionAck, DeliveryAdmissionAckParts};
use grapt_types::remote_mail::*;
use grapt_types::{
    MailboxAdmissionSeq, MailboxBody, MailboxCursor, MailboxMessageId, MailboxSourceCursor,
    ProtocolVersion, SeatMailboxMessage, SeatMessageKind, SeatRecipientId,
};

pub(super) const TAG: &[u8; 6] = b"SEMRM2";
/// A corrupted record cannot allocate an unbounded receipt or reference list.
pub const MAX_REMOTE_MAIL_RECORD_BYTES: usize = 4_194_304;

/// Supplied separately from an incoming row by the authenticated binding owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMailboxScope {
    pub projects: ProjectPair,
    pub principal: SourcePrincipalId,
    pub recipient: SeatRecipientId,
}
impl RemoteMailboxScope {
    /// Preserve the authenticated target and recipient while qualifying a row
    /// by its provider-verified original source project.
    pub fn for_origin(&self, source: SourceProjectId) -> Self {
        Self {
            projects: ProjectPair::new(source, self.projects.target().clone()),
            principal: self.principal.clone(),
            recipient: self.recipient.clone(),
        }
    }
    fn same_recipient_binding(&self, other: &Self) -> bool {
        self.projects.target() == other.projects.target()
            && self.principal == other.principal
            && self.recipient == other.recipient
    }
}

#[derive(Clone, Copy)]
enum ScopeSelection {
    Origin,
    Binding,
}
impl ScopeSelection {
    fn matches(self, expected: &RemoteMailboxScope, actual: &RemoteMailboxScope) -> bool {
        match self {
            Self::Origin => expected == actual,
            Self::Binding => expected.same_recipient_binding(actual),
        }
    }
}

/// Typed input from the strict transport. The store rechecks its own bounds.
#[derive(Debug, Clone)]
pub struct RemoteMailboxInput {
    pub scope: RemoteMailboxScope,
    pub protocol: ProtocolVersion,
    pub source_message: RemoteMailboxMessageId,
    pub source_binding: SourceBindingId,
    pub source_cursor: ExclusiveSourceCursor,
    pub sender: SourcePrincipalId,
    pub kind: SeatMessageKind,
    pub body: MailboxBody,
    pub source_created: SourceTimestamp,
    pub refs: Vec<String>,
    pub supersedes: Option<RemoteMailboxMessageId>,
    pub superseded_by: Option<RemoteMailboxMessageId>,
    pub archive: SourceArchiveDisposition,
    pub attention: MailAttention,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Metadata {
    scope: RemoteMailboxScope,
    ack: DeliveryAdmissionAck,
    source_created: SourceTimestamp,
    sender: SourcePrincipalId,
    refs: Vec<String>,
    supersedes: Option<RemoteMailboxMessageId>,
    superseded_by: Option<RemoteMailboxMessageId>,
    archive: SourceArchiveDisposition,
    attention: MailAttention,
    primary_witnessed: bool,
    projection: Option<SecondaryProjectionState>,
}

/// One verified remote row; delivery and notification consumption are separate.
#[derive(Debug, Clone)]
pub struct RemoteMailboxRecord {
    message: SeatMailboxMessage,
    metadata: Metadata,
    consumed: bool,
    superseded_locally: bool,
}
impl RemoteMailboxRecord {
    pub fn message(&self) -> &SeatMailboxMessage {
        &self.message
    }
    pub fn ack(&self) -> &DeliveryAdmissionAck {
        &self.metadata.ack
    }
    pub fn scope(&self) -> &RemoteMailboxScope {
        &self.metadata.scope
    }
    pub fn sender(&self) -> &SourcePrincipalId {
        &self.metadata.sender
    }
    pub fn source_created(&self) -> &SourceTimestamp {
        &self.metadata.source_created
    }
    pub fn refs(&self) -> &[String] {
        &self.metadata.refs
    }
    /// The withdrawn message in the source's own identity, for a notification
    /// whose `id:` is also a source id. `superseded_by` is not exposed: it is
    /// derived locally, so a replayed capture would change as attention moved.
    pub fn supersedes_source(&self) -> Option<&RemoteMailboxMessageId> {
        self.metadata.supersedes.as_ref()
    }
    pub fn consumed(&self) -> bool {
        self.consumed
    }
    pub fn delivery_pending(&self) -> bool {
        !self.metadata.primary_witnessed
    }
    pub fn projection(&self) -> Option<SecondaryProjectionState> {
        self.metadata.projection
    }
    pub fn repair_pending(&self) -> bool {
        matches!(
            self.metadata.projection,
            Some(SecondaryProjectionState::Refused | SecondaryProjectionState::Unreadable)
        )
    }
    pub fn actionable(&self) -> bool {
        !self.consumed
            && !self.superseded_locally
            && self.metadata.attention == MailAttention::Wake
            && self.metadata.archive == SourceArchiveDisposition::Active
            && self.metadata.superseded_by.is_none()
    }
}

#[derive(Debug)]
pub struct RemoteMailboxPage {
    pub records: Vec<RemoteMailboxRecord>,
    /// Progress through scanned local rows, including quiet/other-scope rows.
    pub next_after: Option<MailboxCursor>,
    pub decoded_bytes: usize,
}

#[derive(Debug)]
pub struct RemoteMailboxCheckpointPage {
    /// Latest matching candidate in this page. Resume is allowed only after
    /// every earlier prefix page validates and `next_before` is exhausted.
    pub candidate: Option<ExclusiveSourceCursor>,
    /// Continue the bounded descending scan; absence never hides an unrun page.
    pub next_before: Option<MailboxAdmissionSeq>,
    pub decoded_bytes: usize,
}

#[derive(Debug)]
pub struct RemoteMailboxAdmission {
    pub admitted: MailboxAdmitted,
    /// Always the originally committed coordinates, even on a new binding.
    pub ack: DeliveryAdmissionAck,
}

/// Which rows a remote page reads, and how deeply it verifies them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryRows {
    /// Every row, fully verified.
    All,
    /// Every row, without re-hashing: candidates for a bounded walk.
    Scan,
    /// Unconsumed rows only, fully verified.
    Open,
    /// Unconsumed rows only, without re-hashing.
    OpenScan,
    /// Rows whose original delivery is still unacknowledged, fully verified.
    Debt,
}

impl HistoryRows {
    /// The SELECT over these rows, oldest first or newest first.
    fn select(self, newest_first: bool) -> (String, Check) {
        // No INDEXED BY: SQLite picks the open or debt index when it exists
        // and the recipient order index otherwise. Same rows, same order.
        let (predicate, check) = match self {
            Self::All => ("", Check::Full),
            Self::Scan => ("", Check::Scan),
            Self::Open => (" AND consumed=0", Check::Full),
            Self::OpenScan => (" AND consumed=0", Check::Scan),
            Self::Debt => (" AND source_acknowledged=0", Check::Full),
        };
        let order = if newest_first {
            "admission_seq<=?2 AND admission_seq>?4 ORDER BY admission_seq DESC"
        } else {
            "admission_seq>?2 ORDER BY admission_seq"
        };
        (
            format!("SELECT message_id,recipient,admission_seq,kind,supersedes,record_bytes FROM mailbox_message WHERE recipient=?1{predicate} AND {order} LIMIT ?3"),
            check,
        )
    }
}

fn refusal(reason: impl Into<String>) -> WriteError {
    WriteError::RemoteMailbox {
        reason: reason.into(),
    }
}
mod codec;
mod presentation;
pub mod progress;
mod read_budget;
mod recent;
mod wake;
pub(super) use codec::Check;
use codec::{
    decode_checked, digest, encode, immutable_bytes, local_id, sender_address, validate_edges,
};
pub use presentation::MailBindingExpectation;
pub use recent::RemoteMailboxRecentPage;
pub use wake::{RemoteMailRecentWakeScan, RemoteMailWakeClaim, RemoteMailWakeScan};
pub(super) fn decode_message_checked(
    bytes: &[u8],
    check: Check,
) -> Result<SeatMailboxMessage, String> {
    codec::decode_message_checked(bytes, check)
}

fn read_record(
    conn: &rusqlite::Connection,
    stored: &StoredRow,
) -> Result<RemoteMailboxRecord, WriteError> {
    read_record_budgeted(conn, stored, None, Check::Full)
}
fn read_record_budgeted(
    conn: &rusqlite::Connection,
    stored: &StoredRow,
    budget: Option<&mut read_budget::RecordByteBudget>,
    check: Check,
) -> Result<RemoteMailboxRecord, WriteError> {
    let (_, seq) = decode_row_checked(stored, check)?;
    let (message, metadata) = decode_checked(&stored.record_bytes, check).map_err(|reason| {
        WriteError::MailboxUndecodable {
            message: stored.message_id.clone(),
            reason,
        }
    })?;
    // Cached: a scan reads this marker once per row it examines.
    let (acknowledged, consumed): (i64, i64) = conn
        .prepare_cached(
            "SELECT source_acknowledged, consumed FROM mailbox_message WHERE message_id = ?1",
        )?
        .query_row([&stored.message_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    if !matches!(acknowledged, 0 | 1)
        || !matches!(consumed, 0 | 1)
        || (acknowledged == 1) != metadata.primary_witnessed
        || seq != metadata.ack.local_admission()
    {
        return Err(refusal(
            "remote admission, delivery or consumption marker disagrees with receipt",
        ));
    }
    // A later correction's backward edge is sufficient even when the older
    // DTO was not fetched again. Validate the successor before it can hide work.
    let successor = read_budget::successor(
        conn,
        metadata.scope.recipient.as_str(),
        message.id().as_str(),
        budget,
    )?;
    let superseded_locally = if let Some(successor) = successor {
        decode_row_checked(&successor, check)?;
        let (_, successor_metadata) =
            decode_checked(&successor.record_bytes, check).map_err(refusal)?;
        if successor_metadata.scope != metadata.scope {
            return Err(refusal("superseding row belongs to another remote scope"));
        }
        true
    } else {
        false
    };
    Ok(RemoteMailboxRecord {
        message,
        metadata,
        consumed: consumed == 1,
        superseded_locally,
    })
}
fn require(
    conn: &rusqlite::Connection,
    scope: &RemoteMailboxScope,
    id: &MailboxMessageId,
) -> Result<RemoteMailboxRecord, WriteError> {
    let record = required_record(conn, id)?;
    if record.scope() != scope {
        return Err(refusal("remote mailbox scope does not match the separate expected principal/projects/recipient"));
    }
    Ok(record)
}
fn require_bound(
    conn: &rusqlite::Connection,
    binding: &RemoteMailboxScope,
    id: &MailboxMessageId,
) -> Result<RemoteMailboxRecord, WriteError> {
    bound_record(binding, required_record(conn, id)?)
}
fn require_bound_budgeted(
    conn: &rusqlite::Connection,
    binding: &RemoteMailboxScope,
    id: &MailboxMessageId,
    budget: &mut read_budget::RecordByteBudget,
) -> Result<RemoteMailboxRecord, WriteError> {
    let stored = read_budget::required_row(conn, id.as_str(), budget)?;
    bound_record(
        binding,
        read_record_budgeted(conn, &stored, Some(budget), Check::Full)?,
    )
}
fn bound_record(
    binding: &RemoteMailboxScope,
    record: RemoteMailboxRecord,
) -> Result<RemoteMailboxRecord, WriteError> {
    if !binding.same_recipient_binding(record.scope()) {
        return Err(refusal(
            "remote mailbox row does not match the authenticated target/principal/recipient",
        ));
    }
    Ok(record)
}
fn required_record(
    conn: &rusqlite::Connection,
    id: &MailboxMessageId,
) -> Result<RemoteMailboxRecord, WriteError> {
    let stored =
        bounded_read_row(conn, id.as_str())?.ok_or_else(|| WriteError::MailboxNotAdmitted {
            message: id.as_str().into(),
        })?;
    read_record(conn, &stored)
}

impl EventStore {
    /// Commit message and exact delivery debt together, before any remote ACK.
    pub fn admit_remote_mailbox(
        &mut self,
        expected: &RemoteMailboxScope,
        input: &RemoteMailboxInput,
    ) -> Result<RemoteMailboxAdmission, WriteError> {
        let tx = self.governed_transaction()?;
        let result = Self::admit_remote_mailbox_within(&tx, expected, input)?;
        tx.commit()?.discard();
        Ok(result)
    }
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn admit_remote_mailbox_within(
        tx: &GovernedTransaction<'_>,
        expected: &RemoteMailboxScope,
        input: &RemoteMailboxInput,
    ) -> Result<RemoteMailboxAdmission, WriteError> {
        if &input.scope != expected {
            return Err(refusal(
                "incoming remote scope does not match the separately authenticated binding",
            ));
        }
        if input.protocol.as_str() != "remote-seat-mail-transport/v1" {
            return Err(refusal("unsupported remote mailbox protocol"));
        }
        let mut input_bytes = input.body.len_bytes().saturating_add(8192);
        for reference in &input.refs {
            input_bytes = input_bytes.saturating_add(reference.len().saturating_add(4));
            if reference.is_empty() || input_bytes > MAX_REMOTE_MAIL_RECORD_BYTES {
                return Err(refusal("invalid or oversized remote references"));
            }
        }
        let id = local_id(expected, &input.source_message)?;
        let existing = bounded_read_row(tx, id.as_str())?;
        let original = match &existing {
            Some(stored) => Some(read_record(tx, stored)?),
            None => None,
        };
        let seq = match &original {
            Some(row) => row.ack().local_admission(),
            None => next_seq(tx, &expected.recipient)?,
        };
        let cursor = original
            .as_ref()
            .map(|row| row.ack().source_cursor())
            .unwrap_or(&input.source_cursor);
        let message = SeatMailboxMessage::new(
            input.protocol.clone(),
            id.clone(),
            sender_address(expected, &input.sender)?,
            expected.recipient.clone(),
            input.kind.clone(),
            input.body.clone(),
            input
                .supersedes
                .as_ref()
                .map(|source| local_id(expected, source))
                .transpose()?,
            MailboxSourceCursor::new(cursor.as_str())
                .ok_or_else(|| refusal("empty source cursor"))?,
        )
        .map_err(|e| refusal(e.to_string()))?;
        let provisional = DeliveryAdmissionAck::new(DeliveryAdmissionAckParts {
            ack_id: DeliveryAdmissionAckId::try_new("pending")
                .map_err(|e| refusal(e.to_string()))?,
            source_binding: input.source_binding.clone(),
            projects: expected.projects.clone(),
            source_message: input.source_message.clone(),
            source_cursor: cursor.clone(),
            recipient: expected.recipient.clone(),
            local_message: id.clone(),
            local_admission: seq,
            canonical_message_digest: MailboxMessageDigest::try_new("pending")
                .map_err(|e| refusal(e.to_string()))?,
            protocol: input.protocol.clone(),
        });
        let mut metadata = Metadata {
            scope: expected.clone(),
            ack: provisional,
            source_created: input.source_created.clone(),
            sender: input.sender.clone(),
            refs: input.refs.clone(),
            supersedes: input.supersedes.clone(),
            superseded_by: input.superseded_by.clone(),
            archive: input.archive,
            attention: input.attention,
            primary_witnessed: false,
            projection: None,
        };
        if metadata.refs.iter().any(String::is_empty) {
            return Err(refusal("empty remote reference"));
        }
        validate_edges(&metadata).map_err(refusal)?;
        let canonical_message_digest = digest(&message, &metadata)?;
        metadata.ack = DeliveryAdmissionAck::new(DeliveryAdmissionAckParts {
            ack_id: DeliveryAdmissionAckId::try_new(format!("delivery:{}", id.as_str()))
                .map_err(|e| refusal(e.to_string()))?,
            source_binding: input.source_binding.clone(),
            projects: expected.projects.clone(),
            source_message: input.source_message.clone(),
            source_cursor: cursor.clone(),
            recipient: expected.recipient.clone(),
            local_message: id.clone(),
            local_admission: seq,
            canonical_message_digest,
            protocol: input.protocol.clone(),
        });
        let admitted = if let Some(original) = original {
            if original.scope() != expected
                || immutable_bytes(&original.message, &original.metadata)
                    != immutable_bytes(&message, &metadata)
            {
                return Err(WriteError::MailboxConflict {
                    message: id.as_str().into(),
                });
            }
            if matches!(
                original.metadata.archive,
                SourceArchiveDisposition::Archived(_)
            ) && metadata.archive == SourceArchiveDisposition::Active
            {
                metadata.archive = original.metadata.archive;
            }
            if metadata.superseded_by.is_none() {
                metadata.superseded_by = original.metadata.superseded_by.clone();
            }
            metadata.ack = original.metadata.ack;
            metadata.primary_witnessed = original.metadata.primary_witnessed;
            metadata.projection = original.metadata.projection;
            MailboxAdmitted::Idempotent { seq }
        } else {
            MailboxAdmitted::Admitted { seq }
        };
        let bytes = encode(&message, &metadata);
        if bytes.len() > MAX_REMOTE_MAIL_RECORD_BYTES {
            return Err(refusal("remote record exceeds its durable byte ceiling"));
        }
        if existing.is_some() {
            tx.gate().execute(
                tx,
                GovernedTable::MailboxMessage,
                "UPDATE mailbox_message SET record_bytes = ?1 WHERE message_id = ?2",
                rusqlite::params![bytes, id.as_str()],
            )?;
        } else {
            let seq_db = i64::try_from(seq.value())
                .map_err(|_| refusal("remote admission sequence exhausted"))?;
            tx.gate().execute(tx, GovernedTable::MailboxMessage, "INSERT INTO mailbox_message (message_id,recipient,admission_seq,kind,supersedes,source_acknowledged,consumed,record_bytes) VALUES (?1,?2,?3,?4,?5,0,0,?6)", rusqlite::params![id.as_str(), expected.recipient.as_str(), seq_db, message.kind().as_str(), message.supersedes().map(|target| target.as_str()), bytes])?;
        }
        Ok(RemoteMailboxAdmission {
            admitted,
            ack: metadata.ack,
        })
    }
    /// Caller supplies the strict port's witnessed success with original coordinates.
    pub fn settle_remote_mailbox_delivery(
        &mut self,
        expected: &RemoteMailboxScope,
        ack: &DeliveryAdmissionAck,
        projection: Option<SecondaryProjectionState>,
    ) -> Result<(), WriteError> {
        let tx = self.governed_transaction()?;
        Self::settle_remote_mailbox_delivery_within(&tx, expected, ack, projection)?;
        tx.commit()?.discard();
        Ok(())
    }
    pub fn settle_remote_mailbox_delivery_within(
        tx: &GovernedTransaction<'_>,
        expected: &RemoteMailboxScope,
        ack: &DeliveryAdmissionAck,
        projection: Option<SecondaryProjectionState>,
    ) -> Result<(), WriteError> {
        let mut record = require(tx, expected, ack.local_message())?;
        if !record.ack().is_exact_repeat_of(ack) {
            return Err(refusal(
                "remote ACK settlement does not match every original coordinate",
            ));
        }
        record.metadata.primary_witnessed = true;
        // An unmeasured receipt cannot erase a previously measured repair debt.
        // The real adapter requires a measurement; the fake may leave it absent.
        if projection.is_some() {
            record.metadata.projection = projection;
        }
        let bytes = encode(&record.message, &record.metadata);
        tx.gate().execute(
            tx,
            GovernedTable::MailboxMessage,
            "UPDATE mailbox_message SET source_acknowledged=1, record_bytes=?1 WHERE message_id=?2",
            rusqlite::params![bytes, ack.local_message().as_str()],
        )?;
        Ok(())
    }
    pub fn consume_remote_mailbox(
        &mut self,
        expected: &RemoteMailboxScope,
        id: &MailboxMessageId,
    ) -> Result<(), WriteError> {
        let tx = self.governed_transaction()?;
        Self::consume_remote_mailbox_within(&tx, expected, id)?;
        tx.commit()?.discard();
        Ok(())
    }
    /// Compose consumption with the existing conversation/window commit.
    /// Handler acceptance alone is not a durable notification receipt.
    pub fn consume_remote_mailbox_within(
        tx: &GovernedTransaction<'_>,
        expected: &RemoteMailboxScope,
        id: &MailboxMessageId,
    ) -> Result<crate::MailboxConsumed, WriteError> {
        let record = require(tx, expected, id)?;
        if record.consumed {
            return Ok(crate::MailboxConsumed::AlreadyConsumed);
        }
        if !record.actionable() {
            return Err(refusal(
                "quiet, archived or superseded remote mail cannot be promoted",
            ));
        }
        tx.gate().execute(
            tx,
            GovernedTable::MailboxMessage,
            "UPDATE mailbox_message SET consumed=1 WHERE message_id=?1",
            [id.as_str()],
        )?;
        Ok(crate::MailboxConsumed::Consumed)
    }
    pub fn read_remote_mailbox(
        &self,
        expected: &RemoteMailboxScope,
        after: MailboxCursor,
        limit: usize,
        max_bytes: usize,
    ) -> Result<RemoteMailboxPage, WriteError> {
        self.read_remote_mailbox_selected(
            expected,
            after,
            (limit, max_bytes),
            ScopeSelection::Origin,
            HistoryRows::All,
        )
    }
    /// Read all verified origins on this exact authenticated recipient's floor.
    /// Row origin remains part of durable identity and original ACK coordinates.
    pub fn read_remote_mailbox_for_binding(
        &self,
        binding: &RemoteMailboxScope,
        after: MailboxCursor,
        limit: usize,
        max_bytes: usize,
    ) -> Result<RemoteMailboxPage, WriteError> {
        self.read_remote_mailbox_for_binding_rows(
            binding,
            after,
            limit,
            max_bytes,
            HistoryRows::All,
        )
    }
    /// The same oldest-first page over the rows `rows` names. Only `All` and
    /// `Debt` are hash-verified; a candidate from `Open` or `Scan` is re-read
    /// in full by whatever acknowledges, settles or captures it.
    pub fn read_remote_mailbox_for_binding_rows(
        &self,
        binding: &RemoteMailboxScope,
        after: MailboxCursor,
        limit: usize,
        max_bytes: usize,
        rows: HistoryRows,
    ) -> Result<RemoteMailboxPage, WriteError> {
        self.read_remote_mailbox_selected(
            binding,
            after,
            (limit, max_bytes),
            ScopeSelection::Binding,
            rows,
        )
    }
    fn read_remote_mailbox_selected(
        &self,
        expected: &RemoteMailboxScope,
        after: MailboxCursor,
        (limit, max_bytes): (usize, usize),
        selection: ScopeSelection,
        which: HistoryRows,
    ) -> Result<RemoteMailboxPage, WriteError> {
        if limit == 0 || max_bytes == 0 {
            return Err(refusal("remote page bounds must be positive"));
        }
        let limit = limit.min(MAX_MAILBOX_PAGE_MESSAGES);
        let (sql, check) = which.select(false);
        let mut statement = self.connection().prepare_cached(&sql)?;
        let mut rows = statement.query(rusqlite::params![
            expected.recipient.as_str(),
            i64::try_from(after.value()).unwrap_or(i64::MAX),
            i64::try_from(limit + 1).unwrap_or(i64::MAX)
        ])?;
        let mut records = Vec::new();
        let mut scanned = 0;
        let mut decoded_bytes = 0usize;
        let mut last = None;
        let mut more = false;
        while let Some(row) = rows.next()? {
            let stored = bounded_row(row)?;
            let (_, seq) = decode_row_checked(&stored, check)?;
            if scanned == limit
                || decoded_bytes.saturating_add(stored.record_bytes.len()) > max_bytes
            {
                if scanned == 0 {
                    return Err(refusal(
                        "one remote recovery row exceeds the current page byte bound",
                    ));
                }
                more = true;
                break;
            }
            scanned += 1;
            decoded_bytes += stored.record_bytes.len();
            last = Some(seq);
            if stored.record_bytes.starts_with(TAG) {
                let record = read_record_budgeted(self.connection(), &stored, None, check)?;
                if selection.matches(expected, record.scope()) {
                    records.push(record);
                }
            }
        }
        Ok(RemoteMailboxPage {
            records,
            next_after: if more {
                last.map(MailboxCursor::after)
            } else {
                None
            },
            decoded_bytes,
        })
    }
    /// Only a settled prefix is resumable; an earlier outstanding row blocks it.
    pub fn remote_mailbox_checkpoint_page(
        &self,
        expected: &RemoteMailboxScope,
        before: Option<MailboxAdmissionSeq>,
        limit: usize,
        max_bytes: usize,
    ) -> Result<RemoteMailboxCheckpointPage, WriteError> {
        self.remote_mailbox_checkpoint_selected(
            expected,
            before,
            limit,
            max_bytes,
            ScopeSelection::Origin,
        )
    }
    /// A resumable cursor covers the settled prefix of every origin on the
    /// receiving binding, while retaining each row's original source cursor.
    pub fn remote_mailbox_checkpoint_page_for_binding(
        &self,
        binding: &RemoteMailboxScope,
        before: Option<MailboxAdmissionSeq>,
        limit: usize,
        max_bytes: usize,
    ) -> Result<RemoteMailboxCheckpointPage, WriteError> {
        self.remote_mailbox_checkpoint_selected(
            binding,
            before,
            limit,
            max_bytes,
            ScopeSelection::Binding,
        )
    }
    fn remote_mailbox_checkpoint_selected(
        &self,
        expected: &RemoteMailboxScope,
        before: Option<MailboxAdmissionSeq>,
        limit: usize,
        max_bytes: usize,
        selection: ScopeSelection,
    ) -> Result<RemoteMailboxCheckpointPage, WriteError> {
        if limit == 0 || max_bytes == 0 {
            return Err(refusal("checkpoint page bound must be positive"));
        }
        let first_pending: Option<i64> = self.connection().query_row("SELECT MIN(admission_seq) FROM mailbox_message WHERE recipient=?1 AND source_acknowledged<>1", [expected.recipient.as_str()], |row| row.get(0))?;
        let bound = first_pending.unwrap_or(i64::MAX).min(
            before
                .and_then(|seq| i64::try_from(seq.value()).ok())
                .unwrap_or(i64::MAX),
        );
        let limit = limit.min(MAX_MAILBOX_PAGE_MESSAGES);
        let mut statement = self.connection().prepare("SELECT message_id,recipient,admission_seq,kind,supersedes,record_bytes FROM mailbox_message WHERE recipient=?1 AND admission_seq<?2 ORDER BY admission_seq DESC LIMIT ?3")?;
        let mut rows = statement.query(rusqlite::params![
            expected.recipient.as_str(),
            bound,
            i64::try_from(limit + 1).unwrap_or(i64::MAX)
        ])?;
        let mut scanned = 0;
        let mut decoded_bytes = 0usize;
        let mut last = None;
        let mut candidate = None;
        while let Some(row) = rows.next()? {
            let stored = bounded_row(row)?;
            let (_, seq) = decode_row(&stored)?;
            if scanned == limit
                || decoded_bytes.saturating_add(stored.record_bytes.len()) > max_bytes
            {
                if scanned == 0 {
                    return Err(refusal(
                        "one checkpoint row exceeds the current page byte bound",
                    ));
                }
                return Ok(RemoteMailboxCheckpointPage {
                    candidate,
                    next_before: last,
                    decoded_bytes,
                });
            }
            scanned += 1;
            decoded_bytes += stored.record_bytes.len();
            last = Some(seq);
            if stored.record_bytes.starts_with(TAG) {
                let record = read_record(self.connection(), &stored)?;
                if selection.matches(expected, record.scope()) {
                    if record.delivery_pending() {
                        return Err(refusal(
                            "checkpoint candidate still has primary delivery debt",
                        ));
                    }
                    if candidate.is_none() {
                        candidate = Some(record.ack().source_cursor().clone());
                    }
                }
            }
        }
        Ok(RemoteMailboxCheckpointPage {
            candidate,
            next_before: None,
            decoded_bytes,
        })
    }
}
fn bounded_row(row: &rusqlite::Row<'_>) -> Result<StoredRow, WriteError> {
    let bytes = row
        .get_ref(5)?
        .as_blob()
        .map_err(|_| refusal("mailbox record is not a blob"))?;
    if bytes.len() > MAX_REMOTE_MAIL_RECORD_BYTES {
        return Err(refusal("mailbox record exceeds recovery byte ceiling"));
    }
    #[cfg(test)]
    MATERIALIZED_REMOTE_BYTES.with(|count| count.set(count.get().saturating_add(bytes.len())));
    Ok(StoredRow {
        message_id: row.get(0)?,
        recipient: row.get(1)?,
        admission_seq: row.get(2)?,
        kind: row.get(3)?,
        supersedes: row.get(4)?,
        record_bytes: bytes.to_vec(),
        // bounded_row reads the data columns strictly (a wrong class refuses
        // there), so it never observes one.
        shape_error: None,
    })
}

fn bounded_read_row(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<Option<StoredRow>, WriteError> {
    let mut statement = conn.prepare("SELECT message_id,recipient,admission_seq,kind,supersedes,record_bytes FROM mailbox_message WHERE message_id=?1")?;
    let mut rows = statement.query([id])?;
    rows.next()?.map(bounded_row).transpose()
}

// Test-only observation at the actual record-copy boundary, shared by ordinary
// and budgeted reads so a budget bypass cannot evade the witness.
#[cfg(test)]
std::thread_local! {
    static MATERIALIZED_REMOTE_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
