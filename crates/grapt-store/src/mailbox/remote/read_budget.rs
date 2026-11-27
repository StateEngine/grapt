//! Charge every record needed for progress validation before copying its body.
use super::{bounded_read_row, bounded_row, refusal, StoredRow, MAX_REMOTE_MAIL_RECORD_BYTES};
use crate::WriteError;
use rusqlite::{Connection, OptionalExtension};

#[derive(Debug)]
pub(super) struct RecordByteBudget {
    maximum: usize,
    used: usize,
}
impl RecordByteBudget {
    pub(super) fn new(maximum: usize) -> Self {
        Self { maximum, used: 0 }
    }
    pub(super) fn used(&self) -> usize {
        self.used
    }
    fn remaining(&self) -> usize {
        self.maximum.saturating_sub(self.used)
    }
    fn charge(&mut self, bytes: usize) -> Result<(), WriteError> {
        self.used = self
            .used
            .checked_add(bytes)
            .filter(|n| *n <= self.maximum)
            .ok_or(WriteError::RemoteMailProgressBudget)?;
        Ok(())
    }
}

pub(super) fn required_row(
    conn: &Connection,
    id: &str,
    budget: &mut RecordByteBudget,
) -> Result<StoredRow, WriteError> {
    let ceiling = i64::try_from(budget.remaining().min(MAX_REMOTE_MAIL_RECORD_BYTES))
        .map_err(|_| refusal("mail progress record byte ceiling"))?;
    // SQLite returns only the length when the record cannot fit. The CASE
    // prevents a large auxiliary body from entering the result before charging.
    let mut statement = conn.prepare(
        "SELECT message_id,recipient,admission_seq,kind,supersedes,
         CASE WHEN length(CAST(record_bytes AS BLOB))<=?2 THEN record_bytes END,
         length(CAST(record_bytes AS BLOB)) FROM mailbox_message WHERE message_id=?1",
    )?;
    let mut rows = statement.query(rusqlite::params![id, ceiling])?;
    let row = rows
        .next()?
        .ok_or_else(|| WriteError::MailboxNotAdmitted { message: id.into() })?;
    let size = usize::try_from(row.get::<_, i64>(6)?)
        .map_err(|_| refusal("invalid mail progress record size"))?;
    if size > MAX_REMOTE_MAIL_RECORD_BYTES {
        return Err(refusal("mailbox record exceeds recovery byte ceiling"));
    }
    budget.charge(size)?;
    bounded_row(row)
}

pub(super) fn successor(
    conn: &Connection,
    recipient: &str,
    id: &str,
    budget: Option<&mut RecordByteBudget>,
) -> Result<Option<StoredRow>, WriteError> {
    // The ordering index scans unrelated recipient history for absent successors.
    // Restrict candidates by both equalities before choosing the earliest one.
    // Cached: a scan looks up one successor per row it examines.
    let successor: Option<Option<String>> = conn.prepare_cached(
        "SELECT CASE WHEN length(CAST(message_id AS BLOB))<=256 THEN message_id END FROM mailbox_message INDEXED BY mailbox_message_supersedes_idx WHERE recipient=?1 AND supersedes=?2 ORDER BY admission_seq LIMIT 1",
    )?.query_row(rusqlite::params![recipient, id], |row| row.get(0)).optional()?;
    let Some(successor) = successor else {
        return Ok(None);
    };
    let id = grapt_types::MailboxMessageId::try_new(
        successor.ok_or_else(|| refusal("superseding message identity exceeds byte bound"))?,
    )
    .map_err(|_| refusal("invalid superseding message identity"))?;
    let stored = match budget {
        Some(budget) => required_row(conn, id.as_str(), budget)?,
        None => {
            bounded_read_row(conn, id.as_str())?.ok_or_else(|| WriteError::MailboxNotAdmitted {
                message: id.as_str().into(),
            })?
        }
    };
    Ok(Some(stored))
}
