//! The write path: append + project in ONE synchronous transaction.
//!
//! Ports the LAW of the oracle's `bus.ts:198-362` commit (projectors run
//! inside the same immediate transaction that inserts the event row, with
//! `event_sequence` as the per-aggregate cursor) — not its code. Measured
//! laws honored: within-aggregate seq is zero-based and
//! strictly increasing but NOT contiguous; `event_sequence.seq` equals
//! `MAX(event.seq)` for every aggregate (305/305 on the real clone).
//!
//! Replay-conflict identity is the FULL identity per the oracle law
//! (bus.ts:255-266): an exact retry of an existing event id must match
//! aggregate, type (name+version), created, AND canonical data to reconcile
//! as an idempotent no-op; any difference is divergence and REJECTS. Data
//! comparison uses the canonical hash (R1 design, byte-validated on all
//! 69,740 real events).
//!
//! That comparison is reached only AFTER the stored row decodes through the
//! same closed decoder as every replay read, so the reconcile-versus-diverge
//! decision can never settle a row the replay path refuses.

use crate::election::WriterOwnerId;
use crate::governed::{GovernedTable, GovernedTransaction};
use rusqlite::Connection;

#[cfg(test)]
use crate::decode::DecodedEvent;
#[cfg(test)]
use crate::election::{Election, ElectionError, LostWriterState};
#[cfg(test)]
use crate::governed::WriteGate;
#[cfg(test)]
use crate::projection::ProjectionId;
#[cfg(test)]
use grapt_event::AggregateId;
#[cfg(test)]
use grapt_types::{EventId, EventVersion, IdAuthority, WriterEpoch};
#[cfg(test)]
use serde_json::Value;

mod error;
mod event_log;
mod hooks;
mod model;
mod open;
mod schema;
mod store;

pub use error::{sqlite_storage_failure, ProjectorError, WriteError};
pub use model::{AppendInput, Appended, CaughtUp, FencedWriter, HeldWriters, Projector};
pub(crate) use open::{ExistingOpenError, FreshBoundary, FreshOpenError};
pub use store::EventStore;

/// Allocates the next sequence for one aggregate and stores the cursor.
///
/// Per-aggregate cursor: zero-based, `event_sequence.seq == MAX(event.seq)`.
/// Corrupt (negative) and exhausted cursors are typed refusals, never wraps or
/// sentinels (review round 2, item B).
///
/// Shared by the session and campaign append paths so there is exactly one
/// place a sequence can be minted. A campaign-local copy would be a second
/// allocator, and "no second canonical write route" would then be true only
/// of the INSERT and not of the cursor it depends on.
pub(crate) fn allocate_sequence(
    tx: &GovernedTransaction<'_>,
    family: &str,
    identity: &str,
) -> Result<u64, WriteError> {
    let next_seq: u64 = match tx.query_row(
        "SELECT seq FROM event_sequence WHERE aggregate_family = ?1 AND aggregate_id = ?2",
        rusqlite::params![family, identity],
        |r| r.get::<_, i64>(0),
    ) {
        Ok(last) => {
            let last = u64::try_from(last).map_err(|_| WriteError::CorruptSequence {
                aggregate: identity.to_owned(),
                value: last,
            })?;
            last.checked_add(1)
                .ok_or_else(|| WriteError::SequenceExhausted {
                    aggregate: identity.to_owned(),
                })?
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => 0,
        Err(e) => return Err(e.into()),
    };
    let next_seq_db = i64::try_from(next_seq).map_err(|_| WriteError::SequenceExhausted {
        aggregate: identity.to_owned(),
    })?;
    tx.gate().execute(
        tx,
        GovernedTable::EventSequence,
        "INSERT INTO event_sequence (aggregate_family, aggregate_id, seq) VALUES (?1, ?2, ?3)
         ON CONFLICT(aggregate_family, aggregate_id) DO UPDATE SET seq = excluded.seq",
        rusqlite::params![family, identity, next_seq_db],
    )?;
    Ok(next_seq)
}

/// The allocated sequence cursor, READ and never advanced.
///
/// Deliberately not [`allocate_sequence`], which is read-AND-increment and
/// already contains this exact `SELECT`. Reaching for it here is the obvious
/// reuse and it is the defect: a purely diagnostic inspection would consume a
/// sequence number, and the consumed number would then be missing from that
/// aggregate forever. A diagnosis that damages what it inspects is worse than
/// no diagnosis. R02 makes exactly that substitution, and the witness catches
/// it by reading the cursor numerically on both sides of the call.
pub(crate) fn read_sequence_cursor(
    tx: &GovernedTransaction<'_>,
    family: &str,
    identity: &str,
) -> Result<Option<u64>, WriteError> {
    let stored: Option<i64> = tx
        .query_row(
            "SELECT seq FROM event_sequence WHERE aggregate_family = ?1 AND aggregate_id = ?2",
            rusqlite::params![family, identity],
            |r| r.get::<_, i64>(0),
        )
        .map(Some)
        .or_else(|e| {
            if e == rusqlite::Error::QueryReturnedNoRows {
                Ok(None)
            } else {
                Err(e)
            }
        })?;
    match stored {
        None => Ok(None),
        Some(value) => u64::try_from(value)
            .map(Some)
            .map_err(|_| WriteError::CorruptSequence {
                aggregate: identity.to_owned(),
                value,
            }),
    }
}

impl EventStore {
    /// Reserves one durable attempt receipt inside an already-open governed
    /// transaction. See [`crate::attempt_receipt`].
    ///
    /// This is the PRE-I/O seam: it runs before the external call, and the
    /// whole-record byte ceiling is decided here, once. A caller that cannot
    /// reserve must not make the call.
    pub fn reserve_attempt_receipt_within(
        tx: &GovernedTransaction<'_>,
        input: &crate::attempt_receipt::ReserveInput<'_>,
    ) -> Result<crate::attempt_receipt::AttemptReceiptReserved, WriteError> {
        crate::attempt_receipt::reserve_within(tx, input)
    }

    /// Reserves one attempt receipt in its own governed transaction.
    ///
    /// No advisory notice is minted: an attempt receipt is durable OPERATIONAL
    /// truth, not a published fact, so the empty committed carrier is discarded
    /// here rather than handed to the caller.
    pub fn reserve_attempt_receipt(
        &mut self,
        input: &crate::attempt_receipt::ReserveInput<'_>,
    ) -> Result<crate::attempt_receipt::AttemptReceiptReserved, WriteError> {
        let tx = self.governed_transaction()?;
        let reserved = Self::reserve_attempt_receipt_within(&tx, input)?;
        tx.commit()?.discard();
        Ok(reserved)
    }

    /// Commits the durable SEND PERMIT for one reserved attempt, inside an
    /// already-open governed transaction.
    ///
    /// Commit this BEFORE entering any adapter boundary that can emit bytes. It
    /// means "the request may have crossed the external boundary from this point
    /// on", never "it did" — see [`crate::attempt_receipt`] for why the
    /// post-send reading is the defect that authorises a duplicate provider
    /// call.
    ///
    /// Takes a [`FencedWriter`] rather than a bare epoch so the caller must have
    /// been elected for the aggregate: a marker write that minted its own epoch
    /// would make the stale-writer refusal unreachable.
    pub fn permit_attempt_send_within(
        tx: &GovernedTransaction<'_>,
        attempt: &grapt_types::PhysicalAttemptId,
        writer: FencedWriter,
        ceiling: crate::attempt_receipt::AttemptRecordCeiling,
        marker: grapt_types::RequestSentMarker,
    ) -> Result<grapt_types::MarkerOutcome, WriteError> {
        tx.require_durable_commit()?;
        crate::attempt_receipt::record_send_permit_within(tx, attempt, writer, ceiling, marker)
    }

    /// Moves one untrustworthy receipt row into the durable quarantine, inside
    /// an already-open governed transaction. See [`crate::attempt_receipt`].
    ///
    /// The transfer is SQL-side, so an oversize record is preserved for audit
    /// without ever being allocated by this process. A caller's cursor may
    /// advance past the row only after this transaction commits.
    pub fn quarantine_attempt_receipt_within(
        tx: &GovernedTransaction<'_>,
        attempt: &grapt_types::PhysicalAttemptId,
        current: FencedWriter,
        reason: crate::attempt_receipt::QuarantineReason,
        detected_at: grapt_types::Timestamp,
    ) -> Result<crate::attempt_receipt::ReceiptQuarantined, WriteError> {
        crate::attempt_receipt::quarantine_within(tx, attempt, current, reason, detected_at)
    }

    /// Settles one lost attempt: appends the exact `ProcessLoss` terminal AND
    /// fills the settled marker in ONE transaction, under the CURRENT elected
    /// epoch. See [`crate::attempt_receipt`].
    ///
    /// This is the recovery route to a settled marker; ordinary settlement has
    /// a second route below for a terminal that is already durable. There is
    /// deliberately no standalone settled write: both routes establish their
    /// terminal authority before reaching the crate-private marker writer.
    pub fn settle_attempt_process_loss_within(
        tx: &GovernedTransaction<'_>,
        attempt: &grapt_types::PhysicalAttemptId,
        current: FencedWriter,
        ceiling: crate::attempt_receipt::AttemptRecordCeiling,
        settled_at: grapt_types::SettledMarker,
        projectors: &mut [&mut dyn Projector],
    ) -> Result<crate::attempt_receipt::AttemptSettlement, WriteError> {
        crate::attempt_receipt::settle_process_loss_within(
            tx, attempt, current, ceiling, settled_at, projectors,
        )
    }

    /// Removes one settled receipt, but only after the terminal Step fact named
    /// by the RECEIPT ITSELF is read back INSIDE this transaction. See
    /// [`crate::attempt_receipt`].
    ///
    /// Deliberately has no own-transaction sibling. The atomic path is this
    /// function called in the transaction that commits the terminal, and a
    /// convenience that opened its own transaction would make that path
    /// unexpressible while looking like the easier choice.
    pub fn reclaim_attempt_receipt_within(
        tx: &GovernedTransaction<'_>,
        attempt: &grapt_types::PhysicalAttemptId,
        writer: FencedWriter,
        ceiling: crate::attempt_receipt::AttemptRecordCeiling,
    ) -> Result<crate::attempt_receipt::AttemptReceiptReclaimed, WriteError> {
        crate::attempt_receipt::reclaim_within(tx, attempt, writer, ceiling)
    }

    /// Settles one receipt from an already-durable ordinary Step terminal and
    /// removes it in the SAME caller-owned transaction. See
    /// [`crate::attempt_receipt`].
    ///
    /// The caller appends the exact terminal first. This seam never mints a
    /// terminal, opens a transaction, or accepts a terminal identity from the
    /// caller: it proves the receipt's own persisted coordinates, records the
    /// settled marker only after that proof, and deletes through the settled
    /// guard. Commit is the single exact-once boundary.
    pub fn settle_attempt_ordinary_terminal_within(
        tx: &GovernedTransaction<'_>,
        attempt: &grapt_types::PhysicalAttemptId,
        writer: FencedWriter,
        ceiling: crate::attempt_receipt::AttemptRecordCeiling,
        settled_at: grapt_types::SettledMarker,
    ) -> Result<crate::attempt_receipt::AttemptReceiptReclaimed, WriteError> {
        crate::attempt_receipt::settle_ordinary_terminal_within(
            tx, attempt, writer, ceiling, settled_at,
        )
    }

    /// Reads one durable attempt receipt by exact attempt id.
    pub fn attempt_receipt(
        &self,
        attempt: &grapt_types::PhysicalAttemptId,
        ceiling: crate::attempt_receipt::AttemptRecordCeiling,
    ) -> Result<Option<crate::attempt_receipt::AttemptReceiptRecord>, WriteError> {
        crate::attempt_receipt::read_one(&self.conn, attempt, ceiling)
    }

    /// Reads the NEXT row a bounded recovery pass must act on — ONE row, after
    /// comparing its declared size against the budget the caller has left.
    ///
    /// `remaining_bytes` is required and injected: this lane owns the per-row
    /// decision, the caller owns the cumulative per-pass budget. The scan
    /// includes SETTLED rows, because a settled receipt whose reclaim never ran
    /// is a leak that an unsettled-only scan could never see. See
    /// [`crate::attempt_receipt`].
    pub fn next_attempt_recovery_row(
        &self,
        session: &grapt_types::SessionId,
        after: Option<&grapt_types::PhysicalAttemptId>,
        remaining_bytes: usize,
        ceiling: crate::attempt_receipt::AttemptRecordCeiling,
    ) -> Result<crate::attempt_receipt::RecoveryStep, WriteError> {
        crate::attempt_receipt::next_recovery_row(
            &self.conn,
            session,
            after,
            remaining_bytes,
            ceiling,
        )
    }

    /// Reads the next distinct Session that owns any live attempt receipt.
    ///
    /// One-row keyset iteration keeps startup recovery bounded without
    /// collecting the entire receipt table or inventing Session ownership in a
    /// consumer crate.
    pub fn next_attempt_recovery_session(
        &self,
        after: Option<&grapt_types::SessionId>,
    ) -> Result<Option<grapt_types::SessionId>, WriteError> {
        crate::attempt_receipt::next_recovery_session(&self.conn, after)
    }

    /// Admits one seat mailbox message inside an already-open governed
    /// transaction, so a caller can settle it atomically with whatever else
    /// belongs to the same durable step. See [`crate::mailbox`].
    ///
    /// `expected` is the recipient the CALLER is admitting for, supplied
    /// separately from the record: a record that named its own recipient would
    /// authorise itself.
    pub fn admit_mailbox_message_within(
        tx: &GovernedTransaction<'_>,
        expected: &grapt_types::SeatRecipientId,
        message: &grapt_types::SeatMailboxMessage,
    ) -> Result<crate::mailbox::MailboxAdmitted, WriteError> {
        crate::mailbox::admit_within(tx, expected, message)
    }

    /// Admits one seat mailbox message in its own governed transaction.
    ///
    /// No advisory notice is minted: received mail is durable OPERATIONAL
    /// truth, not a published fact of this store, so the empty committed
    /// carrier is discarded here rather than handed to the caller.
    pub fn admit_mailbox_message(
        &mut self,
        expected: &grapt_types::SeatRecipientId,
        message: &grapt_types::SeatMailboxMessage,
    ) -> Result<crate::mailbox::MailboxAdmitted, WriteError> {
        let tx = self.governed_transaction()?;
        let admitted = Self::admit_mailbox_message_within(&tx, expected, message)?;
        tx.commit()?.discard();
        Ok(admitted)
    }

    /// Records that the SOURCE has been acknowledged for one admitted message,
    /// inside an already-open governed transaction. See [`crate::mailbox`].
    pub fn acknowledge_mailbox_source_within(
        tx: &GovernedTransaction<'_>,
        expected: &grapt_types::SeatRecipientId,
        id: &grapt_types::MailboxMessageId,
    ) -> Result<crate::mailbox::MailboxAcknowledged, WriteError> {
        crate::mailbox::record_source_acknowledged_within(tx, expected, id)
    }

    /// Records the source acknowledgement in its own governed transaction.
    ///
    /// Requires the durable admission: local admission comes first, so a
    /// message this seat never admitted cannot be acknowledged to its source.
    pub fn acknowledge_mailbox_source(
        &mut self,
        expected: &grapt_types::SeatRecipientId,
        id: &grapt_types::MailboxMessageId,
    ) -> Result<crate::mailbox::MailboxAcknowledged, WriteError> {
        let tx = self.governed_transaction()?;
        let acknowledged = Self::acknowledge_mailbox_source_within(&tx, expected, id)?;
        tx.commit()?.discard();
        Ok(acknowledged)
    }

    /// Marks one admitted message consumed by this seat, inside an already-open
    /// governed transaction. Distinct from the source acknowledgement above.
    /// See [`crate::mailbox`].
    pub fn consume_mailbox_message_within(
        tx: &GovernedTransaction<'_>,
        expected: &grapt_types::SeatRecipientId,
        id: &grapt_types::MailboxMessageId,
    ) -> Result<crate::mailbox::MailboxConsumed, WriteError> {
        crate::mailbox::mark_consumed_within(tx, expected, id)
    }

    /// Marks one admitted message consumed in its own governed transaction.
    pub fn consume_mailbox_message(
        &mut self,
        expected: &grapt_types::SeatRecipientId,
        id: &grapt_types::MailboxMessageId,
    ) -> Result<crate::mailbox::MailboxConsumed, WriteError> {
        let tx = self.governed_transaction()?;
        let consumed = Self::consume_mailbox_message_within(&tx, expected, id)?;
        tx.commit()?.discard();
        Ok(consumed)
    }

    /// Marks one UNREADABLE row consumed in its own governed transaction: the
    /// row an actionable read degraded and named in the page, identified by
    /// its indexed message id verbatim (it may not form a typed identity).
    /// See [`crate::mailbox`].
    pub fn consume_unreadable_mailbox_message(
        &mut self,
        expected: &grapt_types::SeatRecipientId,
        message_id: &str,
    ) -> Result<crate::mailbox::MailboxConsumed, WriteError> {
        let tx = self.governed_transaction()?;
        let consumed = crate::mailbox::mark_consumed_unreadable_within(&tx, expected, message_id)?;
        tx.commit()?.discard();
        Ok(consumed)
    }

    /// One bounded page of a recipient's ACTIONABLE mail, decoded, in ascending
    /// local admission order, strictly after `after`, hard-capped at
    /// [`crate::mailbox::MAX_MAILBOX_PAGE_MESSAGES`] messages and
    /// [`crate::mailbox::MAX_MAILBOX_PAGE_BYTES`] decoded body bytes.
    ///
    /// Actionable excludes consumed and superseded rows. See [`crate::mailbox`].
    pub fn read_actionable_mailbox(
        &self,
        recipient: &grapt_types::SeatRecipientId,
        after: grapt_types::MailboxCursor,
        limit: usize,
    ) -> Result<crate::mailbox::MailboxPage, WriteError> {
        crate::mailbox::read_actionable(&self.conn, recipient, after, limit)
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// This instance's writer identity.
    pub fn owner(&self) -> &WriterOwnerId {
        &self.owner
    }
}

/// Deliberate injection of durable rows the store's own API can never produce.
///
/// Three raw `INSERT` statements used to sit inline in the tests below, minting
/// durable rows behind the append path's back. They are collected here so every
/// such write is named, documented and counted in one place, and `#[cfg(test)]`
/// seals the module out of production builds entirely. A cargo feature would
/// not: features are additive, so any dependent in the graph could switch this
/// on for everyone.
///
/// Since F3B1b these statements also need the store's own [`WriteGate`], because
/// the governed tables they write are refused on the store's connection like any
/// other consumer write. That is the point: staging hostile durable state is a
/// governed act, so it takes the store's lift rather than a public escape.
#[cfg(test)]
pub(crate) mod hostile_rows {
    use super::*;

    /// One stored event row, staged verbatim.
    ///
    /// `writer_epoch` is carried rather than supplied, because both values are
    /// under test: `Some(_)` stages a well-formed fenced row whose CONTENT the
    /// closed decoder refuses, and `None` is the new-v1 write the database
    /// itself must abort.
    pub(crate) struct ForeignEvent<'a> {
        pub(crate) aggregate: &'a AggregateId,
        pub(crate) id: &'a str,
        pub(crate) seq: i64,
        pub(crate) created_ms: i64,
        pub(crate) versioned_type: &'a str,
        pub(crate) data_text: &'a str,
        pub(crate) writer_epoch: Option<i64>,
    }

    /// Writes one stored event row verbatim, bypassing `append_within`.
    pub(crate) fn install_event(
        conn: &Connection,
        gate: &WriteGate,
        row: ForeignEvent<'_>,
    ) -> Result<usize, rusqlite::Error> {
        gate.execute(
            conn,
            GovernedTable::Event,
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                row.id,
                crate::election::family_of(row.aggregate),
                crate::election::identity_of(row.aggregate),
                row.seq,
                row.created_ms,
                row.versioned_type,
                row.data_text,
                row.writer_epoch
            ],
        )
    }

    /// Writes one raw cursor value, including a corrupt (negative) one.
    ///
    /// `event_sequence` carries no trigger: the corruption staged here is what
    /// the append path's typed `CorruptSequence` refusal exists to catch, so a
    /// database-level gate would remove the very state under test.
    /// Writes one raw watermark row, including states the store cannot produce.
    ///
    /// Two are under test: a corrupt (negative) position, and a position PAST
    /// the aggregate's durable head. Neither is reachable through `advance`,
    /// which is exactly why the read side has to refuse them — the database file
    /// is reachable from outside the chokepoint (`F3B1B_RESIDUALS.md` item 2),
    /// so "the writer cannot create this" is not a reason for the reader to
    /// trust it.
    pub(crate) fn install_watermark(
        conn: &Connection,
        gate: &WriteGate,
        projection: crate::projection::ProjectionId,
        aggregate: &AggregateId,
        seq: i64,
    ) -> Result<usize, rusqlite::Error> {
        gate.execute(
            conn,
            GovernedTable::ProjectionWatermark,
            "INSERT INTO projection_watermark (projection, aggregate_family, aggregate_id, seq)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(projection, aggregate_family, aggregate_id) DO UPDATE SET seq = excluded.seq",
            rusqlite::params![
                projection.as_str(),
                crate::election::family_of(aggregate),
                crate::election::identity_of(aggregate),
                seq
            ],
        )
    }

    /// One stored attempt-receipt row, staged verbatim.
    ///
    /// Every indexed column is supplied SEPARATELY from the bytes, which is the
    /// whole point: the store's own write path authors both sides from one
    /// record and therefore cannot produce a disagreement, so the seven
    /// column-versus-bytes refusals have no other route to a witness.
    pub(crate) struct ForeignAttemptReceipt<'a> {
        pub(crate) attempt_id: &'a str,
        pub(crate) session_id: &'a str,
        pub(crate) step: i64,
        pub(crate) ordinal: i64,
        pub(crate) writer_epoch: i64,
        pub(crate) terminal_event_id: &'a str,
        pub(crate) terminal_created_ms: i64,
        pub(crate) reserved_ms: i64,
        pub(crate) request_sent_ms: Option<i64>,
        pub(crate) settled_ms: Option<i64>,
        pub(crate) record_bytes: &'a [u8],
    }

    pub(crate) fn install_attempt_receipt(
        conn: &Connection,
        gate: &WriteGate,
        row: ForeignAttemptReceipt<'_>,
    ) -> Result<usize, rusqlite::Error> {
        // Raw corruption fixtures retain the durability level of a normal
        // governed authority transaction while bypassing only row validation.
        conn.pragma_update(None, "synchronous", "FULL")?;
        gate.execute(
            conn,
            GovernedTable::AttemptReceipt,
            "INSERT INTO attempt_receipt
                (attempt_id, session_id, step, ordinal, writer_epoch,
                 terminal_event_id, terminal_created_ms,
                 reserved_ms, request_sent_ms, settled_ms, record_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                row.attempt_id,
                row.session_id,
                row.step,
                row.ordinal,
                row.writer_epoch,
                row.terminal_event_id,
                row.terminal_created_ms,
                row.reserved_ms,
                row.request_sent_ms,
                row.settled_ms,
                row.record_bytes
            ],
        )
    }

    pub(crate) fn install_cursor(
        conn: &Connection,
        gate: &WriteGate,
        aggregate: &AggregateId,
        seq: i64,
    ) -> Result<usize, rusqlite::Error> {
        gate.execute(
            conn,
            GovernedTable::EventSequence,
            "INSERT INTO event_sequence (aggregate_family, aggregate_id, seq) VALUES (?1, ?2, ?3)
             ON CONFLICT(aggregate_family, aggregate_id) DO UPDATE SET seq = excluded.seq",
            rusqlite::params![
                crate::election::family_of(aggregate),
                crate::election::identity_of(aggregate),
                seq
            ],
        )
    }
}

#[cfg(test)]
mod tests;
