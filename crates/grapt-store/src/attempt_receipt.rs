//! The durable attempt-receipt table: the store's EIGHTH governed table and its
//! third operational (non-event) row family.
//!
//! # What this table is, and is not
//!
//! An attempt receipt is durable operational truth about ONE PhysicalAttempt:
//! that it was reserved, that a send BOUNDARY WAS PERMITTED, and whether it
//! reached a durable terminal.
//!
//! The middle fact is deliberately weaker than it reads at first, and the
//! wording matters. This table cannot know whether bytes left the process —
//! nothing durable can, because the only honest moment to record it is BEFORE
//! the call, and a crash may then happen either side of the wire. What the
//! marker records is a permission granted before a send-capable boundary: from
//! here on, the request MAY have crossed. An earlier revision of this sentence
//! claimed the receipt was truth about "whether bytes left the process", which
//! is a post-send fact this lane owns no carrier for and cannot establish.
//!
//! `request_sent` present with no terminal is therefore AMBIGUOUS and never safe
//! to replay. The end-to-end ordering proof — that the permit is really
//! committed before every send-capable boundary — belongs to R2C5I, not here;
//! this lane owns the carrier and its conservative meaning only.
//!
//! Law: `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT`
//! §"Cancellation ownership" — "R2 writes an operational attempt receipt before
//! the external call. A receipt with `request_sent` and no terminal becomes
//! `AmbiguousProcessLoss`" — and §"Initial rulings", which places it in "an
//! operational `attempt_receipt`, **not automatically in the minimal Session
//! event log**".
//!
//! It is NOT a campaign transition, NOT a session fact and NOT a projection.
//! Nothing here touches the event log's write path, the sequence cursor or any
//! watermark, structurally: no statement in this module writes those tables.
//! One statement READS `event`, and only to prove a terminal exists before a
//! receipt may be removed — a read allocates no sequence and moves no
//! watermark.
//!
//! # No foreign key into the fact log — deliberate
//!
//! `PRAGMA foreign_keys` is ON for this connection, so an FK here would be
//! enforced, and it would also pull this table into the forced
//! `event`/`event_sequence` rebuild the production migration performs. An
//! operational receipt is not a fact-log child. It carries no FK, exactly like
//! `plan_read_receipt` (F4B2a) and `mailbox_message` (R3a), and
//! `tests/production_migration.rs` proves the table and its rows survive that
//! rebuild.
//!
//! # Governed
//!
//! The table is durable truth a consumer must not rewrite: an UPDATE that set
//! `settled_ms` would convert an ambiguous lost attempt into a settled one and
//! authorise a replay the law forbids, and a DELETE would erase the only
//! crash-visible evidence that an external call was ever made. So it joins the
//! governed set and every write here takes the gate's lift for the
//! `AttemptReceipt` table alone. Send permits also retain permanent accounting
//! evidence through its own governed table in the same transaction.
//!
//! (Plain code, not an intra-doc link: the governed vocabulary is crate-private
//! by design, and a link to it fails `rustdoc -D warnings`.)
//!
//! # Bytes are the record, columns are the index
//!
//! The typed receipt persists as exact canonical bytes plus seven indexed
//! columns that are verified against the DECODED bytes before a row is trusted.
//! Every column has a bytes counterpart — including `writer_epoch`, which is
//! why the canonical record is `(receipt, writer_epoch)` and not the A0 receipt
//! alone. A column with no counterpart could not be cross-checked at all, and an
//! unwitnessable column is worse than one more encoded field.
//!
//! # Why the record's encoded length never changes
//!
//! Both later markers are encoded FIXED WIDTH — one presence byte plus eight
//! value bytes, present or not. So a receipt's encoded length is decided by its
//! two identities alone and is identical at reserve, after `request_sent` and
//! after `settled`. That is what makes ruling 8 structural rather than checked:
//! the whole-record byte ceiling is evaluated ONCE, at reserve, pre-I/O, and it
//! already budgets both later marker fields because they are already there.
//! A marker write therefore cannot grow the record and never carries a ceiling
//! at all — it has no size refusal to make. `encoded_len_is_marker_invariant`
//! is the witness.
//!
//! (Plain code, not an intra-doc link: the witness is a `#[cfg(test)]` item and
//! is not in scope for rustdoc, so a link to it fails `RUSTDOCFLAGS=-D warnings`
//! — the exact deny gate this workspace runs as full-gate step 3.)
//!
//! # What this module does NOT own
//!
//! No startup reconciler, no scan scheduling, no election filter, no pass
//! budget and no replay policy: those are B's. No runtime wiring, no provider
//! or adapter handle, and no clock read — every instant is supplied by the
//! caller from its own injected authority. A0 owns the classification law and
//! this module never re-derives it.

use crate::governed::{GovernedTable, GovernedTransaction};
use crate::write::{FencedWriter, WriteError};
use grapt_event::AggregateId;
use grapt_types::{
    MarkerOutcome, PhysicalAttemptId, RequestSentMarker, SessionId, Timestamp, WriterEpoch,
};

#[cfg(test)]
use grapt_types::{
    AttemptOrdinal, BoundedAttemptId, BoundedSessionId, DurableAttemptReceipt, ExternalEffectState,
    LostEffect, ReceiptDisposition, ReservedMarker, SettledMarker, StepNumber,
};
#[cfg(test)]
use std::num::NonZeroU32;

mod lifecycle;
mod record;

pub(crate) use lifecycle::{
    next_recovery_row, read_one, reclaim_within, settle_ordinary_terminal_within,
    settle_process_loss_within,
};
pub use lifecycle::{AttemptReceiptReclaimed, AttemptSettlement};
use record::verify_marker_order;
#[cfg(test)]
use record::FIXED_RECORD_BYTES;
pub use record::{
    decode_record, encode_record, AttemptReceiptRecord, AttemptReceiptReserved,
    AttemptRecordCeiling, QuarantineReason, ReceiptQuarantined, RecoveryRow, RecoveryStep,
    ReserveInput, TerminalCoordinates,
};

pub(crate) fn next_recovery_session(
    conn: &rusqlite::Connection,
    after: Option<&SessionId>,
) -> Result<Option<SessionId>, crate::write::WriteError> {
    let after = after.map(SessionId::as_str).unwrap_or("");
    let found: Option<(String, String)> = conn
        .query_row(
            "SELECT session_id, attempt_id FROM attempt_receipt
             WHERE session_id > ?1
             ORDER BY session_id ASC, attempt_id ASC LIMIT 1",
            [after],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map(Some)
        .or_else(|error| {
            if error == rusqlite::Error::QueryReturnedNoRows {
                Ok(None)
            } else {
                Err(crate::write::WriteError::from(error))
            }
        })?;
    let Some((raw, attempt)) = found else {
        return Ok(None);
    };
    raw.parse::<SessionId>().map(Some).map_err(|_| {
        crate::write::WriteError::ReceiptRecordMismatch {
            attempt,
            detail: format!("the recovery row carries invalid session identity {raw:?}"),
        }
    })
}

pub(crate) fn ensure_schema(conn: &rusqlite::Transaction<'_>) -> Result<(), rusqlite::Error> {
    // No FOREIGN KEY, by ruling — see the module doc. `attempt_id` is the
    // primary key because exact-retry identity IS attempt identity. TABLE ONLY
    // here: the unsettled index is created by [`ensure_index`] AFTER the open
    // path has verified this table's columns, so a wrong-shaped namesake is
    // refused by the missing COLUMN rather than by an index that happens to
    // mention it.
    //
    // `reserved_ms` is NOT NULL and the two later markers are nullable: marker
    // PRESENCE is the classification, structurally, rather than a status field
    // somebody has to remember to update.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS attempt_receipt (
            attempt_id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            step INTEGER NOT NULL,
            ordinal INTEGER NOT NULL,
            writer_epoch INTEGER NOT NULL,
            terminal_event_id TEXT NOT NULL,
            terminal_created_ms INTEGER NOT NULL,
            reserved_ms INTEGER NOT NULL,
            request_sent_ms INTEGER,
            settled_ms INTEGER,
            record_bytes BLOB NOT NULL
        );",
    )
}

/// The durable quarantine for receipt rows that cannot be trusted.
///
/// # Why the raw record is kept
///
/// Declined the smaller identity-and-size table: it destroys the corrupt bytes that make
/// the durable report independently auditable. The runtime path is bounded by SQLite
/// `length()` inspection and an SQL-SIDE transfer, not by deleting evidence — so an
/// oversize blob is moved without ever entering process memory.
///
/// Every identity and terminal coordinate travels with it, so a quarantined row
/// can still be named and audited, plus the closed reason and the epoch that
/// detected it.
pub(crate) fn ensure_quarantine_schema(
    conn: &rusqlite::Transaction<'_>,
) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS attempt_receipt_quarantine (
            attempt_id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            step INTEGER NOT NULL,
            ordinal INTEGER NOT NULL,
            writer_epoch INTEGER NOT NULL,
            terminal_event_id TEXT NOT NULL,
            terminal_created_ms INTEGER NOT NULL,
            reserved_ms INTEGER NOT NULL,
            request_sent_ms INTEGER,
            settled_ms INTEGER,
            record_bytes BLOB NOT NULL,
            reason TEXT NOT NULL,
            declared_bytes INTEGER NOT NULL,
            detected_ms INTEGER NOT NULL,
            detected_epoch INTEGER NOT NULL
        );",
    )
}

/// The recovery scan's index. Runs after column verification — see
/// [`ensure_schema`] for why the split is load-bearing.
///
/// # Why `settled_ms` no longer leads
///
/// It used to, on the reasoning that the only scan was "UNSETTLED receipts for a
/// session I own", and that leading with the session would turn this into a
/// general per-session query surface. The R2C5I correction made recovery
/// settled-INCLUSIVE — a death after the terminal but before the reclaim leaves
/// a settled row that is still owed work — so `settled_ms` is no longer in the
/// predicate at all.
///
/// An index whose leading column the query never constrains cannot serve it.
/// SQLite would have had to scan the table and then sort for the `ORDER BY`,
/// which is precisely the unbounded work a one-row cursor exists to avoid, and
/// the verification would still have reported the index present and correct.
///
/// The columns are exactly and only [`next_recovery_row`]'s predicate and
/// order: equality on `session_id`, range and ordering on `attempt_id`. That is
/// narrower than the old shape rather than wider — it still admits no query the
/// bounded enumeration does not already perform, because a caller must name a
/// session it owns to use it at all.
pub(crate) fn ensure_index(conn: &rusqlite::Transaction<'_>) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS attempt_receipt_recovery
            ON attempt_receipt (session_id, attempt_id);",
    )
}

// ---------------------------------------------------------------------------
// Stored rows
// ---------------------------------------------------------------------------

/// One stored row, raw.
struct StoredRow {
    attempt_id: String,
    session_id: String,
    step: i64,
    ordinal: i64,
    writer_epoch: i64,
    terminal_event_id: String,
    terminal_created_ms: i64,
    reserved_ms: i64,
    request_sent_ms: Option<i64>,
    settled_ms: Option<i64>,
    record_bytes: Vec<u8>,
}

const SELECT_ROW: &str = "SELECT attempt_id, session_id, step, ordinal, writer_epoch,
        terminal_event_id, terminal_created_ms,
        reserved_ms, request_sent_ms, settled_ms, record_bytes
   FROM attempt_receipt WHERE attempt_id = ?1";

fn map_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<StoredRow> {
    Ok(StoredRow {
        attempt_id: r.get(0)?,
        session_id: r.get(1)?,
        step: r.get(2)?,
        ordinal: r.get(3)?,
        writer_epoch: r.get(4)?,
        terminal_event_id: r.get(5)?,
        terminal_created_ms: r.get(6)?,
        reserved_ms: r.get(7)?,
        request_sent_ms: r.get(8)?,
        settled_ms: r.get(9)?,
        record_bytes: r.get(10)?,
    })
}

fn read_row(conn: &rusqlite::Connection, id: &str) -> Result<Option<StoredRow>, WriteError> {
    conn.query_row(SELECT_ROW, [id], map_row)
        .map(Some)
        .or_else(|e| {
            if e == rusqlite::Error::QueryReturnedNoRows {
                Ok(None)
            } else {
                Err(WriteError::from(e))
            }
        })
}

/// Decodes a stored row and refuses ANY disagreement between its indexed
/// columns and the decoded bytes.
///
/// Seven dimensions, each named separately, because a reconciler behaves
/// differently for each and a collapsed message would not say which column
/// moved. The decode comes FIRST: bytes this decoder cannot read are corruption
/// and refuse as [`WriteError::ReceiptUndecodable`], never as a mismatch.
fn decode_row(
    stored: &StoredRow,
    ceiling: AttemptRecordCeiling,
) -> Result<AttemptReceiptRecord, WriteError> {
    let record = decode_record(&stored.record_bytes, ceiling).map_err(|reason| {
        WriteError::ReceiptUndecodable {
            attempt: stored.attempt_id.clone(),
            reason,
        }
    })?;
    let mismatch = |detail: String| WriteError::ReceiptRecordMismatch {
        attempt: stored.attempt_id.clone(),
        detail,
    };
    let receipt = &record.receipt;
    if receipt.attempt().as_str() != stored.attempt_id {
        return Err(mismatch(format!(
            "stored bytes decode to attempt {:?}, not the row's own id",
            receipt.attempt().as_str()
        )));
    }
    if receipt.session().as_str() != stored.session_id {
        return Err(mismatch(format!(
            "stored bytes decode to session {:?}, not the row's session column {:?}",
            receipt.session().as_str(),
            stored.session_id
        )));
    }
    if i64::from(receipt.step().value()) != stored.step {
        return Err(mismatch(format!(
            "stored bytes decode to step {}, not the row's step column {}",
            receipt.step().value(),
            stored.step
        )));
    }
    if i64::from(receipt.ordinal().value()) != stored.ordinal {
        return Err(mismatch(format!(
            "stored bytes decode to ordinal {}, not the row's ordinal column {}",
            receipt.ordinal().value(),
            stored.ordinal
        )));
    }
    if i64::from(record.writer_epoch.value()) != stored.writer_epoch {
        return Err(mismatch(format!(
            "stored bytes decode to writer epoch {}, not the row's epoch column {}",
            record.writer_epoch.value(),
            stored.writer_epoch
        )));
    }
    if record.terminal.event.as_str() != stored.terminal_event_id {
        return Err(mismatch(format!(
            "stored bytes decode to terminal event {:?}, not the row's terminal column {:?}",
            record.terminal.event.as_str(),
            stored.terminal_event_id
        )));
    }
    if record.terminal.created_ms != stored.terminal_created_ms {
        return Err(mismatch(format!(
            "stored bytes decode to terminal instant {}, not the row's terminal-created column {}",
            record.terminal.created_ms, stored.terminal_created_ms
        )));
    }
    if receipt.reserved_at().as_millis() != stored.reserved_ms {
        return Err(mismatch(format!(
            "stored bytes decode to reserve instant {}, not the row's column {}",
            receipt.reserved_at().as_millis(),
            stored.reserved_ms
        )));
    }
    if receipt.request_sent_at().map(Timestamp::as_millis) != stored.request_sent_ms {
        return Err(mismatch(format!(
            "stored bytes decode to request-sent {:?}, not the row's column {:?}",
            receipt.request_sent_at().map(Timestamp::as_millis),
            stored.request_sent_ms
        )));
    }
    if receipt.settled_at().map(Timestamp::as_millis) != stored.settled_ms {
        return Err(mismatch(format!(
            "stored bytes decode to settled {:?}, not the row's column {:?}",
            receipt.settled_at().map(Timestamp::as_millis),
            stored.settled_ms
        )));
    }
    verify_marker_order(&record)?;
    Ok(record)
}

/// Fences one write against the store's own writer ELECTION.
///
/// # Why the election and not the row's own column
///
/// The row's `writer_epoch` records which owner reserved it. Comparing a caller
/// against THAT alone would let the original owner keep writing after ownership
/// moved, because the owner and the row agree with each other — they were
/// written by the same process. Fencing means comparing against the currently
/// elected epoch, which is the authority the whole store already fences appends
/// with, so this path takes the SAME fence rather than inventing a second,
/// weaker one.
///
/// A caller whose epoch is not the elected one is refused with the election's
/// own `StaleWriter`, before anything is written.
fn fence_against_election(
    tx: &GovernedTransaction<'_>,
    session: &SessionId,
    writer: FencedWriter,
) -> Result<(), WriteError> {
    let aggregate = AggregateId::Session(session.clone());
    crate::election::fence(tx, &aggregate, writer.epoch())?;
    Ok(())
}

/// The stored row must exist, decode, agree with its columns, and have been
/// reserved under the epoch the caller holds.
///
/// The epoch check is LAST of the four, deliberately: a stale writer must be
/// told it is stale, not told a row is corrupt because its own view is old.
///
/// EQUALITY here, not "at least": a marker records something this owner did. An
/// owner elected after the reserve did not make that call and may not describe
/// it — the lost attempt is R3's continuation problem, not a marker to be
/// back-filled by whoever holds the aggregate now.
fn require_row(
    conn: &rusqlite::Connection,
    attempt: &PhysicalAttemptId,
    writer_epoch: WriterEpoch,
    ceiling: AttemptRecordCeiling,
) -> Result<(StoredRow, AttemptReceiptRecord), WriteError> {
    let Some(stored) = read_row(conn, attempt.as_str())? else {
        return Err(WriteError::ReceiptNotReserved {
            attempt: attempt.as_str().to_owned(),
        });
    };
    let record = decode_row(&stored, ceiling)?;
    if record.writer_epoch != writer_epoch {
        return Err(WriteError::ReceiptStaleEpoch {
            attempt: attempt.as_str().to_owned(),
            held: record.writer_epoch.value(),
            offered: writer_epoch.value(),
        });
    }
    Ok((stored, record))
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

/// Reserves one receipt inside an already-open governed transaction.
///
/// This is the pre-I/O seam: the ONLY place a whole-record size is decided, and
/// it is decided before any statement runs. A record above the ceiling refuses
/// with [`WriteError::ReceiptRecordTooLarge`] having touched nothing, so an
/// oversize identity costs a refusal to RESERVE — before an external call — and
/// never a refusal to reconcile after one.
///
/// Exact retry reconciles ONLY byte-identical canonical data. A different record
/// under the same attempt id refuses typed and writes nothing.
pub(crate) fn reserve_within(
    tx: &GovernedTransaction<'_>,
    input: &ReserveInput<'_>,
) -> Result<AttemptReceiptReserved, WriteError> {
    let record = AttemptReceiptRecord::new(
        input.receipt.clone(),
        input.writer.epoch(),
        input.terminal.clone(),
    );
    verify_marker_order(&record)?;
    let bytes = encode_record(&record);
    let attempt = record.receipt.attempt().as_str().to_owned();

    // BEFORE any I/O. Nothing above this line has read or written the database,
    // so a caller that cannot reserve learns it without a round trip and
    // certainly without having made the external call.
    if bytes.len() > input.ceiling.as_bytes() {
        return Err(WriteError::ReceiptRecordTooLarge {
            attempt,
            bytes: bytes.len(),
            ceiling: input.ceiling.get(),
        });
    }

    // Fenced against the ELECTION, like every other governed write in this
    // store. A reserve is the pre-I/O admission to make an external call; an
    // owner that has lost the aggregate must not be admitted to make one.
    fence_against_election(tx, record.receipt.session(), input.writer)?;

    if let Some(stored) = read_row(tx, &attempt)? {
        // Decode BEFORE comparing: a stored row this decoder cannot read is
        // corruption and refuses as such, never as a divergence.
        decode_row(&stored, input.ceiling)?;
        if stored.record_bytes == bytes {
            return Ok(AttemptReceiptReserved::Idempotent);
        }
        return Err(WriteError::ReceiptConflict { attempt });
    }

    let receipt = &record.receipt;
    tx.gate().execute(
        tx,
        GovernedTable::AttemptReceipt,
        "INSERT INTO attempt_receipt
            (attempt_id, session_id, step, ordinal, writer_epoch,
             terminal_event_id, terminal_created_ms,
             reserved_ms, request_sent_ms, settled_ms, record_bytes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rusqlite::params![
            attempt,
            receipt.session().as_str(),
            i64::from(receipt.step().value()),
            i64::from(receipt.ordinal().value()),
            i64::from(record.writer_epoch.value()),
            record.terminal.event.as_str(),
            record.terminal.created_ms,
            receipt.reserved_at().as_millis(),
            receipt.request_sent_at().map(Timestamp::as_millis),
            receipt.settled_at().map(Timestamp::as_millis),
            bytes
        ],
    )?;
    Ok(AttemptReceiptReserved::Reserved)
}

/// Commits the durable SEND PERMIT for this attempt.
///
/// # This is committed BEFORE the send, and it is conservative
///
/// R2C5I finding 1, and the sharpest defect this lane had. A marker meaning
/// "bytes have left" can only be written AFTER they leave — and a process that
/// dies between the send and that write reopens holding a reserved-only receipt,
/// which A0 classifies `NoneInitiated` with `safe_to_replay = true`. A replay
/// then duplicates a provider call that already happened.
///
/// So this is committed BEFORE entering any adapter boundary that can emit
/// bytes, and it means **"the request may have crossed the external boundary
/// from this point on"** — never "it did". Conservative is the whole point:
/// sent-without-settled stays `Ambiguous` and never safe to replay, and the
/// permit-but-no-send case is counted ambiguous too, because the durable facts
/// cannot distinguish it from the one-call case and must not pretend to.
///
/// A0's `RequestSentMarker` type is PRESERVED: the fix is the ORDER and the name, not a
/// second marker type that would change A0's classification grid. Nothing in this crate
/// is named for a post-send commit.
///
/// Synchronous, fenced, and carrying no size decision of its own — both markers
/// are fixed width, so this re-encodes a record of exactly the length reserve
/// already admitted. See the module doc.
pub(crate) fn record_send_permit_within(
    tx: &GovernedTransaction<'_>,
    attempt: &PhysicalAttemptId,
    writer: FencedWriter,
    ceiling: AttemptRecordCeiling,
    marker: RequestSentMarker,
) -> Result<MarkerOutcome, WriteError> {
    apply_send_permit(tx, attempt, writer, ceiling, marker.at())
}

/// The one place the send permit reaches the row.
///
/// There is no public sibling for the settled marker. The recovery route writes
/// it after appending its terminal in the same transaction; the ordinary route
/// writes it only after proving its already-durable terminal. Both reach one
/// crate-private writer, while a generic marker dispatcher would make a
/// standalone settled write nameable again.
///
/// The three-armed outcome is A0's [`MarkerOutcome`] and this path does not
/// re-derive it: the stored record is decoded into the A0 type and A0's own
/// consuming transition decides. So the durable law and the in-memory law cannot
/// drift into two rules.
///
/// A conflict writes NOTHING and returns the held value beside the offered one,
/// which is the store half of the O1 law A0 established.
fn apply_send_permit(
    tx: &GovernedTransaction<'_>,
    attempt: &PhysicalAttemptId,
    writer: FencedWriter,
    ceiling: AttemptRecordCeiling,
    at: Timestamp,
) -> Result<MarkerOutcome, WriteError> {
    let (_, record) = require_row(tx, attempt, writer.epoch(), ceiling)?;
    // Fenced against the ELECTION as well as against the row: the row alone
    // agrees with the owner that wrote it, so an owner that lost the aggregate
    // would otherwise keep marking its own receipt after ownership moved.
    fence_against_election(tx, record.receipt.session(), writer)?;
    let writer_epoch = writer.epoch();
    let (updated, outcome) = record
        .receipt
        .clone()
        .mark_request_sent(RequestSentMarker::new(at));
    match outcome {
        // Nothing changed, so nothing is written. `ExactRetry` after a crashed
        // write is normal; `Conflict` is a defect and is reported with both
        // instants rather than collapsed into the held one.
        MarkerOutcome::ExactRetry | MarkerOutcome::Conflict { .. } => return Ok(outcome),
        MarkerOutcome::Recorded => {}
    }
    let updated = AttemptReceiptRecord::new(updated, writer_epoch, record.terminal.clone());
    // The order law is checked on what would be STORED, not only on what was
    // offered: a caller supplying a settled instant before its own reserve is
    // refused here rather than persisted and refused on the next read.
    verify_marker_order(&updated)?;
    crate::provider_attempt_accounting::retain(tx, &updated)?;
    let bytes = encode_record(&updated);
    // Column AND bytes in ONE statement. Two statements could commit a row whose
    // column moved and whose bytes did not, which is the exact disagreement the
    // read path refuses — and a self-inflicted corruption is still corruption.
    tx.gate().execute(
        tx,
        GovernedTable::AttemptReceipt,
        "UPDATE attempt_receipt SET request_sent_ms = ?2, record_bytes = ?3
           WHERE attempt_id = ?1",
        rusqlite::params![attempt.as_str(), at.as_millis(), bytes],
    )?;
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// Durable quarantine
// ---------------------------------------------------------------------------

/// The columns carried verbatim from the live row into the quarantine.
const QUARANTINE_CARRIED: &str = "attempt_id, session_id, step, ordinal, writer_epoch,
     terminal_event_id, terminal_created_ms, reserved_ms, request_sent_ms,
     settled_ms, record_bytes";

/// Moves one untrustworthy receipt row into the durable quarantine.
///
/// # The blob never enters process memory
///
/// The transfer is `INSERT ... SELECT` followed by `DELETE`, both inside the
/// caller's ONE governed transaction. SQLite copies the record between tables
/// itself, so an oversize row is preserved for audit without this process ever
/// allocating it — which is the whole reason quarantine can be bounded at all.
/// Nothing here hashes the payload either: a full-payload digest is never
/// computed in memory just to write the quarantine.
///
/// # Ordering, and what survives a failure
///
/// The source row is removed ONLY after the insert and its verification have
/// both succeeded. Everything is in one transaction, so a failure at any point
/// leaves the live row exactly where it was and the quarantine empty. A caller's
/// cursor may advance only after this commits — never before.
///
/// # Idempotence versus conflict
///
/// An identical quarantine record — same reason, byte-identical record — is
/// [`ReceiptQuarantined::AlreadyQuarantined`]. A quarantine row that exists but
/// DISAGREES is a loud typed refusal, never an overwrite: overwriting would
/// destroy the first durable report of a corruption, which is the one artifact
/// this table exists to keep. The comparison is made SQL-side, so it does not
/// materialize either blob.
///
/// The comparison differs by which state the retry finds, and BOTH compare:
///
/// * the live row is still present — a retry after a failed or rolled-back
///   move. Reason and the two blobs are compared directly;
/// * the live row is gone — a retry after a SUCCEEDED move, which is the common
///   case, since the move deletes the source. Reason is compared, and the held
///   row is required to be internally whole. An offered-versus-held blob
///   comparison is impossible here for anyone: the move removed the only other
///   operand.
///
/// Neither state returns idempotent success without comparing something. That
/// is not a refinement — an earlier revision compared NOTHING once the source
/// was absent, so a different-reason retry passed.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(crate) fn quarantine_within(
    tx: &GovernedTransaction<'_>,
    attempt: &PhysicalAttemptId,
    current: FencedWriter,
    reason: QuarantineReason,
    detected_at: Timestamp,
) -> Result<ReceiptQuarantined, WriteError> {
    // Identity only. The record bytes are deliberately NOT selected.
    let live: Option<String> = tx
        .query_row(
            "SELECT session_id FROM attempt_receipt WHERE attempt_id = ?1",
            [attempt.as_str()],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| {
            if e == rusqlite::Error::QueryReturnedNoRows {
                Ok(None)
            } else {
                Err(WriteError::from(e))
            }
        })?;

    let already: Option<String> = tx
        .query_row(
            "SELECT reason FROM attempt_receipt_quarantine WHERE attempt_id = ?1",
            [attempt.as_str()],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| {
            if e == rusqlite::Error::QueryReturnedNoRows {
                Ok(None)
            } else {
                Err(WriteError::from(e))
            }
        })?;

    let Some(session_id) = live else {
        // Nothing live. Either a previous pass already moved it — idempotent —
        // or there was never a row to quarantine.
        //
        // This branch is the one a completed move actually lands in, because the
        // move REMOVED the live row. An earlier revision returned
        // AlreadyQuarantined here for any held row, comparing nothing, so a
        // retry offering a DIFFERENT reason succeeded and read as idempotent —
        // silently disagreeing with the durable report it claimed to match.
        return match already {
            Some(held) => {
                if held != reason.as_str() {
                    return Err(WriteError::ReceiptQuarantineConflict {
                        attempt: attempt.as_str().to_owned(),
                        held,
                        offered: reason.as_str().to_owned(),
                    });
                }
                // The byte half of "same reason and bytes". With the source row
                // gone there is no offered blob left to compare against, so the
                // available check is that the HELD row is internally whole: its
                // stored blob is exactly the length its own declared_bytes
                // records. A truncated or partially written quarantine row is
                // therefore never reported as a satisfied exact retry.
                //
                // Stated plainly because it is weaker than the live-row case
                // below, which compares the two blobs directly: once the source
                // is removed, an offered-versus-held byte comparison has no
                // second operand and cannot be performed by anyone.
                let whole: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM attempt_receipt_quarantine
                      WHERE attempt_id = ?1 AND declared_bytes = length(record_bytes)",
                    [attempt.as_str()],
                    |r| r.get(0),
                )?;
                if whole == 0 {
                    return Err(WriteError::ReceiptQuarantineConflict {
                        attempt: attempt.as_str().to_owned(),
                        held: format!(
                            "{held} (held record length disagrees with its declared_bytes)"
                        ),
                        offered: reason.as_str().to_owned(),
                    });
                }
                Ok(ReceiptQuarantined::AlreadyQuarantined)
            }
            None => Err(WriteError::ReceiptNotReserved {
                attempt: attempt.as_str().to_owned(),
            }),
        };
    };

    let session: SessionId = session_id
        .parse()
        .map_err(|_| WriteError::ReceiptRecordMismatch {
            attempt: attempt.as_str().to_owned(),
            detail: format!("the row's session column {session_id:?} is not a session identity"),
        })?;
    fence_against_election(tx, &session, current)?;

    if let Some(existing) = already {
        // SQL-side identity comparison: same reason AND byte-identical record,
        // decided by SQLite without either blob crossing into this process.
        let identical: i64 = tx.query_row(
            "SELECT COUNT(*) FROM attempt_receipt_quarantine q
               JOIN attempt_receipt r ON r.attempt_id = q.attempt_id
              WHERE q.attempt_id = ?1 AND q.reason = ?2 AND q.record_bytes = r.record_bytes",
            rusqlite::params![attempt.as_str(), reason.as_str()],
            |r| r.get(0),
        )?;
        if identical == 0 {
            return Err(WriteError::ReceiptQuarantineConflict {
                attempt: attempt.as_str().to_owned(),
                held: existing,
                offered: reason.as_str().to_owned(),
            });
        }
        // Identical: finish the interrupted move rather than writing again.
        tx.gate().execute(
            tx,
            GovernedTable::AttemptReceipt,
            "DELETE FROM attempt_receipt WHERE attempt_id = ?1",
            [attempt.as_str()],
        )?;
        return Ok(ReceiptQuarantined::AlreadyQuarantined);
    }

    let moved = tx.gate().execute(
        tx,
        GovernedTable::AttemptReceiptQuarantine,
        &format!(
            "INSERT INTO attempt_receipt_quarantine
                ({QUARANTINE_CARRIED}, reason, declared_bytes, detected_ms, detected_epoch)
             SELECT {QUARANTINE_CARRIED}, ?2, length(record_bytes), ?3, ?4
               FROM attempt_receipt WHERE attempt_id = ?1"
        ),
        rusqlite::params![
            attempt.as_str(),
            reason.as_str(),
            detected_at.as_millis(),
            i64::from(current.epoch().value())
        ],
    )?;
    if moved != 1 {
        return Err(WriteError::ReceiptRecordMismatch {
            attempt: attempt.as_str().to_owned(),
            detail: format!("the quarantine transfer moved {moved} row(s), not exactly one"),
        });
    }
    // VERIFY before removing the source, still SQL-side.
    let verified: i64 = tx.query_row(
        "SELECT COUNT(*) FROM attempt_receipt_quarantine q
           JOIN attempt_receipt r ON r.attempt_id = q.attempt_id
          WHERE q.attempt_id = ?1 AND q.record_bytes = r.record_bytes
            AND q.declared_bytes = length(r.record_bytes)",
        [attempt.as_str()],
        |r| r.get(0),
    )?;
    if verified != 1 {
        return Err(WriteError::ReceiptRecordMismatch {
            attempt: attempt.as_str().to_owned(),
            detail: "the quarantined copy does not match the live row; the source is retained"
                .to_owned(),
        });
    }
    tx.gate().execute(
        tx,
        GovernedTable::AttemptReceipt,
        "DELETE FROM attempt_receipt WHERE attempt_id = ?1",
        [attempt.as_str()],
    )?;
    Ok(ReceiptQuarantined::Quarantined)
}

#[cfg(test)]
mod tests;
