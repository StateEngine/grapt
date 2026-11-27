//! The durable seat-mailbox table: the store's SEVENTH governed table and its
//! second operational (non-event) row family.
//!
//! # What this table is, and is not
//!
//! A mailbox row is durable operational truth about COORDINATION — a message
//! addressed to one seat, its local admission position, whether the source has
//! been told we have it, and whether the runtime has consumed it. It is not a
//! campaign transition, not a session fact and not a projection: nothing here
//! touches the event log, the sequence cursor or any watermark, structurally,
//! because no statement in this module names those tables.
//!
//! # No foreign key into the fact log — deliberate
//!
//! `PRAGMA foreign_keys` is ON for this connection, so an FK here would be
//! enforced, and it would also pull this table into any later
//! `event`/`event_sequence` rebuild. A coordination row is not a fact-log
//! child. It carries no FK, exactly like `plan_read_receipt` (F4B2a).
//!
//! # Governed
//!
//! The table is durable truth a consumer must not rewrite: an UPDATE that
//! flipped `source_acknowledged` would tell a transport we have a message we do
//! not, and a DELETE would silently drop admitted work. So it joins the
//! governed set and every write here takes the gate's lift for
//! `GovernedTable::MailboxMessage` alone.
//!
//! (Plain code, not an intra-doc link: the governed vocabulary is crate-private
//! by design, and a link to it fails `rustdoc -D warnings` — the exact
//! regression F3B1b was held on.)
//!
//! # Bytes are the record, columns are the index
//!
//! The typed message persists as exact canonical bytes plus four indexed
//! columns (message id, recipient, kind, supersedes) that are verified against
//! the DECODED bytes before a row is trusted. A column and its bytes cannot
//! disagree silently: a row that cannot be decoded is NAMED in the page as
//! an unreadable placeholder rather than vanishing from it, because a mailbox
//! that quietly drops a message is worse than one that reports it cannot be
//! read. A column holding a storage class no record can hold is unreadable
//! for that reason alone, named the same way. A contradiction a placeholder
//! cannot stand for (a row the local cursor cannot order, a remote record in
//! a local table) still refuses the page.
//!
//! # Ordering law
//!
//! Local admission comes FIRST and source acknowledgement second, and that
//! order is the crash-safety argument:
//!
//! * crash before admission — the message is absent and unacknowledged, so the
//!   source still owes it and replay is safe;
//! * crash after admission, before acknowledgement — replay finds the exact
//!   same bytes under the same id, reconciles to the ORIGINAL admission
//!   sequence rather than minting a second one, and the acknowledgement can
//!   then proceed;
//! * crash after acknowledgement — the row is already admitted, so a replay
//!   cannot create a duplicate admission.
//!
//! Acknowledging the SOURCE is a different fact from local CONSUMPTION: the
//! first says "we have it durably", the second says "the runtime has taken it".
//! Collapsing them would make a crash between them either lose work or replay
//! it. R3b owns the consuming side; the marker exists here so that page reads
//! can exclude consumed rows while a crashed consumer can still reconcile
//! through the exact message id.
//!
//! # What this module does NOT own
//!
//! No subscription, no drain loop, no wake, no notification, no safe-boundary
//! promotion, and no StateEngine transport: there is no supported adapter in
//! this workspace and this module fabricates none. Those are R3b/R3c and the
//! separately admitted versioned adapter.

mod record;
pub mod remote;

use crate::governed::{GovernedTable, GovernedTransaction};
use crate::write::WriteError;
use grapt_types::{
    MailboxAdmissionSeq, MailboxCursor, MailboxMessageId, SeatMailboxMessage, SeatRecipientId,
};
use record::{decode_record_checked, encode_record};

#[cfg(test)]
#[path = "../../../test-support/evidence.rs"]
mod mailbox_evidence;

/// The most messages one bounded page may return, whatever the caller asks for.
///
/// Pinned as its own literal, independently of every other bound: a page cap
/// derived from a body cap would move silently when either changed.
pub const MAX_MAILBOX_PAGE_MESSAGES: usize = 64;

/// The most DECODED body bytes one bounded page may return.
///
/// Also its own literal. The two page bounds are independent: a page stops at
/// whichever it reaches first, and neither implies the other.
pub const MAX_MAILBOX_PAGE_BYTES: usize = 262_144;

/// How an admission settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxAdmitted {
    /// The row was written at this local position.
    Admitted { seq: MailboxAdmissionSeq },
    /// A byte-identical record already existed under this id. Nothing was
    /// written and the ORIGINAL position is returned — a retry must not move a
    /// message in the recipient's order.
    Idempotent { seq: MailboxAdmissionSeq },
}

impl MailboxAdmitted {
    /// The local position this message occupies either way.
    pub fn seq(self) -> MailboxAdmissionSeq {
        match self {
            MailboxAdmitted::Admitted { seq } | MailboxAdmitted::Idempotent { seq } => seq,
        }
    }
}

/// How a source acknowledgement settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxAcknowledged {
    Acknowledged,
    /// Already acknowledged; nothing was written. Idempotent by contract, so a
    /// transport that retries after a lost reply cannot corrupt the row.
    AlreadyAcknowledged,
}

/// How a local consumption marker settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxConsumed {
    Consumed,
    /// Already consumed; nothing was written.
    AlreadyConsumed,
}

/// One row of a bounded actionable page, in the row's own admission
/// position.
///
/// A row this build cannot decode is neither dropped nor an error: it stands
/// in the page as [`Self::Unreadable`], named where it is, so one bad row
/// costs that one position and the page keeps answering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailboxPageEntry {
    /// The row's bytes decoded and its indexed columns verified.
    Message(SeatMailboxMessage),
    /// The row's bytes, or their agreement with the indexed columns, could
    /// not be decoded; the row stands named in place of its message.
    Unreadable(MailboxUnreadableRow),
}

/// One durable row the actionable read could not decode, with enough evidence
/// to find it.
///
/// The id and kind are the row's INDEXED columns, verbatim: they may
/// disagree with the bytes (the disagreement is often the reason) and they
/// may be over the identity bounds, so they are carried as plain strings and
/// never reconstructed into typed identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxUnreadableRow {
    /// The row's indexed message id, verbatim.
    pub message: String,
    /// The row's indexed kind label, verbatim; empty when the column's
    /// storage class is not TEXT, which `reason` names.
    pub kind: String,
    /// The row's local admission position.
    pub seq: MailboxAdmissionSeq,
    /// The decoder's own refusal text, carried for the reader's log.
    pub reason: String,
}

/// One bounded page of actionable mail for one recipient.
#[derive(Debug)]
pub struct MailboxPage {
    /// Page rows in ascending local admission order: decoded messages and the
    /// named placeholders of the rows that could not be decoded.
    pub entries: Vec<MailboxPageEntry>,
    /// The exclusive cursor to pass as `after` for the next page when this page
    /// stopped at a bound. `None` means the recipient's actionable mail is
    /// exhausted — a short page ends honestly rather than pretending there is
    /// more.
    pub next_after: Option<MailboxCursor>,
    /// Decoded body bytes emitted by this page, for the caller's own accounting.
    pub body_bytes: usize,
}

pub(crate) fn ensure_schema(conn: &rusqlite::Transaction<'_>) -> Result<(), rusqlite::Error> {
    // No FOREIGN KEY, by ruling — see the module doc. `message_id` is the
    // primary key because exact-retry identity is message identity. TABLE ONLY
    // here: the indexes are created by [`ensure_indexes`] AFTER the open path
    // has verified this table's columns, so that on a wrong-shaped namesake the
    // column check gets first refusal instead of an index complaining about a
    // column that is missing for a different reason.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mailbox_message (
            message_id TEXT PRIMARY KEY,
            recipient TEXT NOT NULL,
            admission_seq INTEGER NOT NULL,
            kind TEXT NOT NULL,
            supersedes TEXT,
            source_acknowledged INTEGER NOT NULL,
            consumed INTEGER NOT NULL,
            record_bytes BLOB NOT NULL
        );",
    )
}

/// The page read's indexes. Runs after column verification — see
/// [`ensure_schema`] for why the split is load-bearing.
pub(crate) fn ensure_indexes(conn: &rusqlite::Transaction<'_>) -> Result<(), rusqlite::Error> {
    // UNIQUE: two rows at one local position for one recipient would make the
    // exclusive cursor ambiguous and could hide a message behind its twin.
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS mailbox_message_recipient_seq_idx
            ON mailbox_message (recipient, admission_seq);
         CREATE INDEX IF NOT EXISTS mailbox_message_supersedes_idx
            ON mailbox_message (recipient, supersedes);",
    )
}

impl crate::EventStore {
    /// Builds the open-row and delivery-debt indexes. The service runs this
    /// after readiness, never at open: on a large store the first index build
    /// reads the whole table, which outlasted the launch window. Every read
    /// works without them; SQLite picks them up once they exist.
    pub fn build_mailbox_scan_indexes(&mut self) -> Result<(), WriteError> {
        let tx = self.governed_transaction()?;
        for sql in [
            "CREATE INDEX IF NOT EXISTS mailbox_message_open_idx
                ON mailbox_message (recipient, consumed, admission_seq)",
            "CREATE INDEX IF NOT EXISTS mailbox_message_debt_idx
                ON mailbox_message (recipient, source_acknowledged, admission_seq)",
        ] {
            tx.gate()
                .execute(&tx, GovernedTable::MailboxMessage, sql, [])?;
        }
        tx.commit()?.discard();
        Ok(())
    }
}

/// One stored row, raw.
struct StoredRow {
    message_id: String,
    recipient: String,
    admission_seq: i64,
    kind: String,
    supersedes: Option<String>,
    record_bytes: Vec<u8>,
    /// The first data column whose storage class is not the one the record
    /// format promises, if any. Such a row cannot agree with ANY record, so
    /// [`decode_row`] refuses it with this reason before touching the bytes:
    /// the row degrades to a named placeholder instead of refusing the page.
    shape_error: Option<String>,
}

/// Reads one row of the mailbox table.
///
/// The identity columns — message id, recipient, admission position — are
/// read strictly: a row that cannot be named or ordered is a STRUCTURAL
/// contradiction, and it refuses the page exactly as an undecodable row
/// without a position does. The data columns — kind, supersedes, record
/// bytes — are read tolerantly: a wrong storage class marks the row
/// unreadable (a named placeholder) rather than an extraction error, so one
/// malformed row cannot refuse the page or its neighbours.
fn extract_row(r: &rusqlite::Row) -> Result<StoredRow, rusqlite::Error> {
    let DataColumns {
        kind,
        supersedes,
        record_bytes,
        shape_error,
    } = read_data_columns(r)?;
    Ok(StoredRow {
        message_id: r.get(0)?,
        recipient: r.get(1)?,
        admission_seq: r.get(2)?,
        kind,
        supersedes,
        record_bytes,
        shape_error,
    })
}

/// One superseder CANDIDATE, read outside the page's own range. Identity and
/// position are read tolerantly there too: a candidate that cannot be named
/// or ordered cannot stand as a vouched superseder, but it must not refuse a
/// page it does not appear in — `Ok(None)` is that "cannot stand" answer,
/// while a real query error still propagates. Identity text is decoded
/// STRICTLY: bytes that are not valid UTF-8 cannot name a message, so the
/// candidate stands for nothing instead of repairing its claim.
fn extract_row_for_claims(r: &rusqlite::Row) -> Result<Option<StoredRow>, rusqlite::Error> {
    use rusqlite::types::ValueRef;
    let message_id = match r.get_ref(0)? {
        ValueRef::Text(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => text.to_owned(),
            Err(_) => return Ok(None),
        },
        _ => return Ok(None),
    };
    let recipient = match r.get_ref(1)? {
        ValueRef::Text(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => text.to_owned(),
            Err(_) => return Ok(None),
        },
        _ => return Ok(None),
    };
    let admission_seq = match r.get_ref(2)? {
        ValueRef::Integer(value) => value,
        _ => return Ok(None),
    };
    let DataColumns {
        kind,
        supersedes,
        record_bytes,
        shape_error,
    } = read_data_columns(r)?;
    Ok(Some(StoredRow {
        message_id,
        recipient,
        admission_seq,
        kind,
        supersedes,
        record_bytes,
        shape_error,
    }))
}

/// Tolerant read of the data columns: `kind` is TEXT, `supersedes` is TEXT
/// or NULL, and `record_bytes` is a BLOB. A column with another storage
/// class yields a placeholder value plus the first reason found, so
/// [`decode_row`] can name the row unreadable instead of letting the typed
/// extraction refuse the page. TEXT columns are decoded STRICTLY: a lossy
/// repair could make a corrupt column agree with the record it corrupts, so
/// invalid UTF-8 is itself the unreadable reason. A non-BLOB record is
/// carried as NO bytes at all: its content is not the record, and only BLOB
/// bytes may be mistaken for a remote tag by the tag checks that do not
/// decode first.
/// The row's data columns after the tolerant read: `kind`, `supersedes`,
/// the raw `record_bytes`, and the first shape error found, if any.
struct DataColumns {
    kind: String,
    supersedes: Option<String>,
    record_bytes: Vec<u8>,
    shape_error: Option<String>,
}

fn read_data_columns(r: &rusqlite::Row) -> Result<DataColumns, rusqlite::Error> {
    use rusqlite::types::ValueRef;

    let mut shape_error = None;
    let kind = match r.get_ref(3)? {
        ValueRef::Text(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => text.to_owned(),
            Err(_) => {
                shape_error = Some("kind column is not valid UTF-8".to_owned());
                String::new()
            }
        },
        other => {
            shape_error = Some(format!("kind column is {:?}, not TEXT", other.data_type()));
            String::new()
        }
    };
    let supersedes = match r.get_ref(4)? {
        ValueRef::Null => None,
        ValueRef::Text(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => Some(text.to_owned()),
            Err(_) => {
                if shape_error.is_none() {
                    shape_error = Some("supersedes column is not valid UTF-8".to_owned());
                }
                None
            }
        },
        other => {
            if shape_error.is_none() {
                shape_error = Some(format!(
                    "supersedes column is {:?}, not TEXT",
                    other.data_type()
                ));
            }
            None
        }
    };
    let record_bytes = match r.get_ref(5)? {
        ValueRef::Blob(bytes) => bytes.to_vec(),
        other => {
            if shape_error.is_none() {
                shape_error = Some(format!(
                    "record_bytes column is {:?}, not BLOB",
                    other.data_type()
                ));
            }
            Vec::new()
        }
    };
    Ok(DataColumns {
        kind,
        supersedes,
        record_bytes,
        shape_error,
    })
}

/// Decodes a stored row and refuses ANY disagreement between its indexed
/// columns and the decoded bytes.
fn decode_row(stored: &StoredRow) -> Result<(SeatMailboxMessage, MailboxAdmissionSeq), WriteError> {
    decode_row_checked(stored, remote::Check::Full)
}

fn decode_row_checked(
    stored: &StoredRow,
    check: remote::Check,
) -> Result<(SeatMailboxMessage, MailboxAdmissionSeq), WriteError> {
    let undecodable = |reason: String| WriteError::MailboxUndecodable {
        message: stored.message_id.clone(),
        reason,
    };
    // A wrong storage class is refused before the bytes are even attempted:
    // no record exists whose columns those values could be, so the bytes are
    // not worth guessing at.
    if let Some(reason) = &stored.shape_error {
        return Err(undecodable(reason.clone()));
    }
    let message = decode_record_checked(&stored.record_bytes, check).map_err(undecodable)?;
    if message.id().as_str() != stored.message_id {
        return Err(undecodable(format!(
            "stored bytes decode to message {:?}, not the row's own id",
            message.id().as_str()
        )));
    }
    if message.recipient().as_str() != stored.recipient {
        return Err(undecodable(format!(
            "stored bytes decode to recipient {:?}, not the row's recipient column {:?}",
            message.recipient().as_str(),
            stored.recipient
        )));
    }
    if message.kind().as_str() != stored.kind {
        return Err(undecodable(format!(
            "stored bytes decode to kind {:?}, not the row's kind column {:?}",
            message.kind().as_str(),
            stored.kind
        )));
    }
    if message.supersedes().map(|id| id.as_str()) != stored.supersedes.as_deref() {
        return Err(undecodable(format!(
            "stored bytes decode to supersedes {:?}, not the row's column {:?}",
            message.supersedes().map(|id| id.as_str()),
            stored.supersedes
        )));
    }
    let seq = u64::try_from(stored.admission_seq)
        .ok()
        .and_then(MailboxAdmissionSeq::new)
        .ok_or_else(|| {
            undecodable(format!(
                "admission sequence {} is not a position",
                stored.admission_seq
            ))
        })?;
    Ok((message, seq))
}

fn read_row(conn: &rusqlite::Connection, id: &str) -> Result<Option<StoredRow>, WriteError> {
    conn.query_row(
        "SELECT message_id, recipient, admission_seq, kind, supersedes, record_bytes
         FROM mailbox_message WHERE message_id = ?1",
        [id],
        extract_row,
    )
    .map(Some)
    .or_else(|e| {
        if e == rusqlite::Error::QueryReturnedNoRows {
            Ok(None)
        } else {
            Err(WriteError::from(e))
        }
    })
}

/// The next local position for this recipient: one past the highest it holds.
fn next_seq(
    tx: &GovernedTransaction<'_>,
    recipient: &SeatRecipientId,
) -> Result<MailboxAdmissionSeq, WriteError> {
    let highest: Option<i64> = tx.query_row(
        "SELECT MAX(admission_seq) FROM mailbox_message WHERE recipient = ?1",
        [recipient.as_str()],
        |r| r.get(0),
    )?;
    match highest {
        None => Ok(MailboxAdmissionSeq::FIRST),
        Some(value) => u64::try_from(value)
            .ok()
            .and_then(MailboxAdmissionSeq::new)
            .ok_or_else(|| WriteError::MailboxCorruptSequence {
                recipient: recipient.as_str().to_owned(),
                value,
            })?
            .next()
            .ok_or_else(|| WriteError::MailboxSequenceExhausted {
                recipient: recipient.as_str().to_owned(),
            }),
    }
}

/// Admits one message into `expected`'s durable mailbox.
///
/// `expected` is supplied SEPARATELY from the record on purpose: a caller that
/// read the recipient out of the record would authorise whatever the record
/// claimed. A record addressed elsewhere refuses typed and writes nothing.
pub(crate) fn admit_within(
    tx: &GovernedTransaction<'_>,
    expected: &SeatRecipientId,
    message: &SeatMailboxMessage,
) -> Result<MailboxAdmitted, WriteError> {
    if message.recipient() != expected {
        return Err(WriteError::MailboxWrongRecipient {
            message: message.id().as_str().to_owned(),
            expected: expected.as_str().to_owned(),
            found: message.recipient().as_str().to_owned(),
        });
    }
    let bytes = encode_record(message);
    let id = message.id().as_str();

    if let Some(stored) = read_row(tx, id)? {
        // Decode BEFORE comparing: a stored row this decoder cannot read is
        // corruption and refuses as such, never as a divergence.
        let (_, seq) = decode_row(&stored)?;
        if stored.record_bytes == bytes {
            return Ok(MailboxAdmitted::Idempotent { seq });
        }
        return Err(WriteError::MailboxConflict {
            message: id.to_owned(),
        });
    }

    let seq = next_seq(tx, expected)?;

    if let Some(target) = message.supersedes() {
        // The target must EXIST and belong to the SAME recipient. "Strictly
        // earlier" needs no separate arm and gets none: `seq` is one past this
        // recipient's highest position, so every existing same-recipient row is
        // below it by construction, and a target that does not exist yet is
        // exactly the missing-target refusal. An arm that cannot fire would be
        // a check nobody can witness — see R3A_RESIDUALS.md item 1, which
        // records the ordering as structure-carried and names the test that
        // asserts the invariant.
        let Some(target_row) = read_row(tx, target.as_str())? else {
            return Err(WriteError::MailboxSupersedesMissing {
                message: id.to_owned(),
                target: target.as_str().to_owned(),
            });
        };
        if target_row.record_bytes.starts_with(remote::TAG) {
            return Err(WriteError::RemoteMailbox {
                reason: "local mail cannot supersede scoped remote mail".into(),
            });
        }
        if target_row.recipient != expected.as_str() {
            return Err(WriteError::MailboxSupersedesCrossRecipient {
                message: id.to_owned(),
                target: target.as_str().to_owned(),
            });
        }
    }

    let seq_db = i64::try_from(seq.value()).map_err(|_| WriteError::MailboxSequenceExhausted {
        recipient: expected.as_str().to_owned(),
    })?;
    tx.gate().execute(
        tx,
        GovernedTable::MailboxMessage,
        "INSERT INTO mailbox_message
            (message_id, recipient, admission_seq, kind, supersedes,
             source_acknowledged, consumed, record_bytes)
         VALUES (?1, ?2, ?3, ?4, ?5, 0, 0, ?6)",
        rusqlite::params![
            id,
            expected.as_str(),
            seq_db,
            message.kind().as_str(),
            message.supersedes().map(|target| target.as_str()),
            bytes
        ],
    )?;
    Ok(MailboxAdmitted::Admitted { seq })
}

/// Records that the SOURCE has been told this message is durably ours.
///
/// Requires the durable admission: acknowledging a message we did not admit
/// would tell a transport to stop resending work we do not have. Idempotent, so
/// a retried acknowledgement after a lost reply is not an error.
pub(crate) fn record_source_acknowledged_within(
    tx: &GovernedTransaction<'_>,
    expected: &SeatRecipientId,
    id: &MailboxMessageId,
) -> Result<MailboxAcknowledged, WriteError> {
    let stored = require_admitted(tx, expected, id)?;
    if stored.record_bytes.starts_with(remote::TAG) {
        return Err(WriteError::RemoteMailbox {
            reason: "remote mail requires scoped delivery/consumption APIs".into(),
        });
    }
    let acknowledged: i64 = tx.query_row(
        "SELECT source_acknowledged FROM mailbox_message WHERE message_id = ?1",
        [stored.message_id.as_str()],
        |r| r.get(0),
    )?;
    if acknowledged != 0 {
        return Ok(MailboxAcknowledged::AlreadyAcknowledged);
    }
    tx.gate().execute(
        tx,
        GovernedTable::MailboxMessage,
        "UPDATE mailbox_message SET source_acknowledged = 1 WHERE message_id = ?1",
        [stored.message_id.as_str()],
    )?;
    Ok(MailboxAcknowledged::Acknowledged)
}

/// Marks one message consumed by this seat's runtime.
///
/// Distinct from the source acknowledgement above: this is OUR side of the
/// handover and is what excludes a row from later actionable pages. Idempotent
/// through the exact message id, so a consumer that crashed after taking the
/// message and before recording it reconciles rather than double-counting.
pub(crate) fn mark_consumed_within(
    tx: &GovernedTransaction<'_>,
    expected: &SeatRecipientId,
    id: &MailboxMessageId,
) -> Result<MailboxConsumed, WriteError> {
    let stored = require_admitted(tx, expected, id)?;
    if stored.record_bytes.starts_with(remote::TAG) {
        return Err(WriteError::RemoteMailbox {
            reason: "remote mail requires scoped delivery/consumption APIs".into(),
        });
    }
    let consumed: i64 = tx.query_row(
        "SELECT consumed FROM mailbox_message WHERE message_id = ?1",
        [stored.message_id.as_str()],
        |r| r.get(0),
    )?;
    if consumed != 0 {
        return Ok(MailboxConsumed::AlreadyConsumed);
    }
    tx.gate().execute(
        tx,
        GovernedTable::MailboxMessage,
        "UPDATE mailbox_message SET consumed = 1 WHERE message_id = ?1",
        [stored.message_id.as_str()],
    )?;
    Ok(MailboxConsumed::Consumed)
}

/// The row must exist AND belong to the expected recipient, AND its bytes
/// must decode against its indexed columns.
fn require_admitted(
    conn: &rusqlite::Connection,
    expected: &SeatRecipientId,
    id: &MailboxMessageId,
) -> Result<StoredRow, WriteError> {
    let stored = require_admitted_undecoded(conn, expected, id.as_str())?;
    // Decoded here too: a row whose columns and bytes disagree may not be
    // acknowledged or consumed on the strength of its columns alone.
    decode_row(&stored)?;
    Ok(stored)
}

/// The row must exist AND belong to the expected recipient.
///
/// No decode cross-check: the only caller is the consumption of a row the
/// actionable read has ALREADY degraded, and that read is the authority that
/// this exact row is unreadable. Every other path keeps the decoded
/// [`require_admitted`].
fn require_admitted_undecoded(
    conn: &rusqlite::Connection,
    expected: &SeatRecipientId,
    id: &str,
) -> Result<StoredRow, WriteError> {
    let Some(stored) = read_row(conn, id)? else {
        return Err(WriteError::MailboxNotAdmitted {
            message: id.to_owned(),
        });
    };
    if stored.recipient != expected.as_str() {
        return Err(WriteError::MailboxWrongRecipient {
            message: id.to_owned(),
            expected: expected.as_str().to_owned(),
            found: stored.recipient,
        });
    }
    Ok(stored)
}

/// Marks one UNREADABLE row consumed: the row an actionable read degraded and
/// named in the page.
///
/// The row stays durable byte for byte; this only records that the runtime
/// has taken its named absence, so the mailbox moves on and one bad row can
/// never hold the reader at its cursor again.
pub(crate) fn mark_consumed_unreadable_within(
    tx: &GovernedTransaction<'_>,
    expected: &SeatRecipientId,
    message_id: &str,
) -> Result<MailboxConsumed, WriteError> {
    let stored = require_admitted_undecoded(tx, expected, message_id)?;
    if stored.record_bytes.starts_with(remote::TAG) {
        return Err(WriteError::RemoteMailbox {
            reason: "remote mail requires scoped delivery/consumption APIs".into(),
        });
    }
    let consumed: i64 = tx.query_row(
        "SELECT consumed FROM mailbox_message WHERE message_id = ?1",
        [message_id],
        |r| r.get(0),
    )?;
    if consumed != 0 {
        return Ok(MailboxConsumed::AlreadyConsumed);
    }
    tx.gate().execute(
        tx,
        GovernedTable::MailboxMessage,
        "UPDATE mailbox_message SET consumed = 1 WHERE message_id = ?1",
        [message_id],
    )?;
    Ok(MailboxConsumed::Consumed)
}

/// Answers, for ONE candidate row, whether it is hidden by a VERIFIED
/// supersession: some same-recipient row whose indexed supersedes column
/// names the candidate, and that row must decode with every indexed column
/// agreeing with its own bytes. One bounded indexed lookup answers for one
/// candidate — never a history-sized query — and a candidate that cannot be
/// named, ordered, or decoded stands for nothing: its claim is read, not
/// vouched, and the row it claims is not hidden.
fn is_superseded_verified(
    conn: &rusqlite::Connection,
    recipient: &str,
    message_id: &str,
) -> Result<bool, WriteError> {
    let mut statement = conn.prepare(
        "SELECT message_id, recipient, admission_seq, kind, supersedes, record_bytes
         FROM mailbox_message
         WHERE recipient = ?1 AND supersedes = ?2",
    )?;
    let claims = statement.query_map([recipient, message_id], extract_row_for_claims)?;
    for claim in claims {
        let Some(stored) = claim? else {
            continue;
        };
        if decode_row(&stored).is_ok() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// One bounded page of `recipient`'s actionable mail, ascending, strictly after
/// `after`.
///
/// Actionable EXCLUDES consumed rows and rows an existing same-recipient
/// message VERIFIEDLY supersedes: the superseder must decode and its indexed
/// supersedes column must agree with its own bytes, because an unreadable
/// row's supersedes column is a claim this build does not vouch for, and an
/// unvouched claim must not keep hiding readable mail — not before the
/// unreadable row is consumed and not after. Every returned row is decoded
/// and its indexed columns verified; a row that cannot be decoded (bytes or
/// column class) stands in the page as [`MailboxPageEntry::Unreadable`],
/// named where it is, rather than refusing the page or vanishing from it.
pub(crate) fn read_actionable(
    conn: &rusqlite::Connection,
    recipient: &SeatRecipientId,
    after: MailboxCursor,
    limit: usize,
) -> Result<MailboxPage, WriteError> {
    if limit == 0 {
        return Err(WriteError::MailboxPageLimitZero);
    }
    let limit = limit.min(MAX_MAILBOX_PAGE_MESSAGES);
    let after_db = i64::try_from(after.value()).unwrap_or(i64::MAX);
    // One more than the cap is fetched so "the page filled" and "the mail ran
    // out" are distinguishable facts rather than an inference from the count.
    // Verified-superseded rows are skipped AFTER the fetch, so a full batch
    // can still hide the next real row behind it; a SHORT batch is what says
    // the mail ran out, and the loop refetches past skipped rows.
    let fetch = i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX);

    let mut entries = Vec::new();
    let mut body_bytes = 0usize;
    let mut last_seq = None;
    let mut more = false;
    let mut batch_after = after_db;
    'page: loop {
        let mut statement = conn.prepare(
            "SELECT message_id, recipient, admission_seq, kind, supersedes, record_bytes
             FROM mailbox_message
             WHERE recipient = ?1
               AND admission_seq > ?2
               AND consumed = 0
             ORDER BY admission_seq ASC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            rusqlite::params![recipient.as_str(), batch_after, fetch],
            extract_row,
        )?;
        let mut batch_len = 0i64;
        let mut batch_last = batch_after;
        for row in rows {
            let stored = row?;
            batch_len += 1;
            batch_last = stored.admission_seq;
            // A row may hide its target only while a VERIFIED superseder can
            // stand for the claim; one bounded indexed lookup answers for
            // this row alone, never for the recipient's whole history.
            if is_superseded_verified(conn, recipient.as_str(), &stored.message_id)? {
                continue;
            }
            // Decoded BEFORE any bound decision: a row that cannot be read must
            // still be classified here, not skipped because the page was full
            // anyway.
            match decode_row(&stored) {
                Ok((message, seq)) => {
                    if stored.record_bytes.starts_with(remote::TAG) {
                        return Err(WriteError::RemoteMailbox {
                            reason: "remote mail requires scoped presentation".into(),
                        });
                    }
                    if entries.len() == limit {
                        more = true;
                        break 'page;
                    }
                    let next_bytes = body_bytes.saturating_add(message.body().len_bytes());
                    if next_bytes > MAX_MAILBOX_PAGE_BYTES && !entries.is_empty() {
                        // Progress is guaranteed for one valid row: the bound only
                        // stops a page that has already emitted something, so a
                        // single oversize body can never wedge the mailbox.
                        more = true;
                        break 'page;
                    }
                    body_bytes = next_bytes;
                    last_seq = Some(seq);
                    entries.push(MailboxPageEntry::Message(message));
                }
                Err(WriteError::MailboxUndecodable { reason, .. }) => {
                    // A row the local cursor cannot order is a STRUCTURAL
                    // contradiction, not a decode failure: it still refuses the
                    // page, because a placeholder cannot stand where no position
                    // exists.
                    let seq = match u64::try_from(stored.admission_seq)
                        .ok()
                        .and_then(MailboxAdmissionSeq::new)
                    {
                        Some(seq) => seq,
                        None => {
                            return Err(WriteError::MailboxUndecodable {
                                message: stored.message_id,
                                reason,
                            })
                        }
                    };
                    if entries.len() == limit {
                        more = true;
                        break 'page;
                    }
                    // One undecodable row costs that one position: it is named
                    // where it stands and the page keeps answering.
                    last_seq = Some(seq);
                    entries.push(MailboxPageEntry::Unreadable(MailboxUnreadableRow {
                        message: stored.message_id,
                        kind: stored.kind,
                        seq,
                        reason,
                    }));
                }
                Err(other) => return Err(other),
            }
        }
        if batch_len < fetch {
            break;
        }
        batch_after = batch_last;
    }

    let next_after = if more {
        last_seq.map(MailboxCursor::after)
    } else {
        None
    };
    Ok(MailboxPage {
        entries,
        next_after,
        body_bytes,
    })
}

#[cfg(test)]
mod evidence_tests {
    use super::{decode_row, extract_row, mailbox_evidence};
    use rusqlite::{Connection, OpenFlags};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    #[ignore = "requires the governed frozen-clone evidence database"]
    fn every_existing_mailbox_row_obeys_the_identity_bounds() -> TestResult {
        let path = mailbox_evidence::frozen_clone();
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let table_count: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'mailbox_message'",
            [],
            |row| row.get(0),
        )?;
        if table_count == 0 {
            eprintln!("governed frozen clone contains no mailbox_message table; audited 0 rows");
            return Ok(());
        }

        let mut statement = conn.prepare(
            "SELECT message_id, recipient, admission_seq, kind, supersedes, record_bytes
             FROM mailbox_message ORDER BY message_id",
        )?;
        let rows = statement.query_map([], extract_row)?;
        let mut audited = 0usize;
        for row in rows {
            let stored = row?;
            if let Err(error) = decode_row(&stored) {
                panic!(
                    "governed frozen-clone mailbox row {} violates the admitted identity bounds: {error}",
                    stored.message_id
                );
            }
            audited = audited.saturating_add(1);
        }
        eprintln!("audited {audited} governed frozen-clone mailbox row(s)");
        Ok(())
    }
}
