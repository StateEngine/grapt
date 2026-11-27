use super::{
    decode_row, encode_record, fence_against_election, read_row, verify_marker_order,
    AttemptReceiptRecord, AttemptRecordCeiling, QuarantineReason, RecoveryRow, RecoveryStep,
    StoredRow,
};
use crate::governed::{GovernedTable, GovernedTransaction};
use crate::write::{FencedWriter, Projector, WriteError};
use grapt_types::{
    ExternalEffectState, LostEffect, PhysicalAttemptId, ReceiptDisposition, SessionId,
    SettledMarker,
};
mod partial_text;
mod recovery_terminal;
/// The durable name of the Rust-native Step FAILURE terminal.
const STEP_FAILED_TERMINAL: &str = "session.step.failed.2";
const STEP_FAILED_TERMINAL_V3: &str = "session.step.failed.3";

/// The durable name of the Rust-native Step SUCCESS terminal.
const STEP_ENDED_TERMINAL: &str = "session.step.ended.2";

/// The aggregate family a Step terminal belongs to.
const SESSION_FAMILY: &str = "session";

/// How a retention call settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptReceiptReclaimed {
    /// The row existed, was settled, its terminal was proven inside this
    /// transaction, and it is gone.
    Reclaimed,
    /// The row was already absent. Typed idempotence, not an error: a reclaim
    /// pass that crashed after its delete committed must be able to re-run.
    AlreadyAbsent,
}

// ---------------------------------------------------------------------------
// Recovery settlement
// ---------------------------------------------------------------------------

/// How a recovery settlement settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptSettlement {
    /// The exact terminal was appended and the settled marker filled, together.
    Settled,
    /// The exact terminal already existed and the receipt was already settled.
    /// Idempotent: a recovery pass that crashed after committing must re-run
    /// without appending a second terminal or changing any event byte.
    AlreadySettled,
}

/// Settles one lost attempt: appends the exact `ProcessLoss` terminal AND fills
/// the settled marker, in ONE transaction.
///
/// # Why there is no standalone production route to a settled marker
///
/// R2C5I finding 2. A standalone settled-marker write can commit and then die
/// before its terminal, leaving a receipt that reads `AlreadySettled` with no
/// terminal anywhere — invisible to an unsettled-only scan, and therefore work
/// that is silently lost. Removing that path is what makes the state
/// unreachable rather than merely discouraged: both production routes reach
/// the same crate-private marker writer only after terminal authority exists.
///
/// # Why it takes the CURRENT epoch, not the reserving one
///
/// R2C5I finding 4. Recovery runs in a later process that has been elected
/// afresh, so it necessarily holds a NEWER epoch than the row it is settling.
/// Demanding the reserve epoch here would make every crashed attempt
/// permanently unsettleable. What is demanded instead is stricter in the way
/// that matters: the caller must be the CURRENTLY ELECTED writer (the store's
/// own election fence, no self-election), and the row's own reserve epoch must
/// not be from the future — a row claiming an epoch beyond the current election
/// is corruption, not lineage.
///
/// # The terminal is the one reserved, never a fresh identity
///
/// The `EventId` and `created_ms` come from the row, so a restart reproduces the
/// EXACT event bytes rather than minting a replacement. That is what keeps the
/// terminal count at one across any number of recovery passes.
///
/// The effect recorded is A0's own classification of the surviving markers —
/// this function does not re-derive it, and in particular a permit with no
/// settlement is `Ambiguous` and never `NoneInitiated`.
pub(crate) fn settle_process_loss_within(
    tx: &GovernedTransaction<'_>,
    attempt: &PhysicalAttemptId,
    current: FencedWriter,
    ceiling: AttemptRecordCeiling,
    settled_at: SettledMarker,
    projectors: &mut [&mut dyn Projector],
) -> Result<AttemptSettlement, WriteError> {
    let Some(stored) = read_row(tx, attempt.as_str())? else {
        return Err(WriteError::ReceiptNotReserved {
            attempt: attempt.as_str().to_owned(),
        });
    };
    let record = decode_row(&stored, ceiling)?;
    // The store's own election fence. No self-election on the write path.
    fence_against_election(tx, record.receipt.session(), current)?;
    // Lineage, not equality: the reservation may be older than the current
    // election, and must be. A row from the future is corruption.
    if record.writer_epoch > current.epoch() {
        return Err(WriteError::ReceiptStaleEpoch {
            attempt: attempt.as_str().to_owned(),
            held: record.writer_epoch.value(),
            offered: current.epoch().value(),
        });
    }

    // Already settled: prove the exact terminal is really there and stop. This
    // is where a legacy or hostile settled-without-terminal row is caught, and
    // it refuses rather than reporting a completion nobody can point at.
    if record.receipt.settled_at().is_some() {
        prove_terminal_within(tx, &record)?;
        return Ok(AttemptSettlement::AlreadySettled);
    }

    if recovery_terminal::reuse(tx, &record)? {
        write_settled_marker(tx, &record, settled_at)?;
        return Ok(AttemptSettlement::Settled);
    }
    let evidence_record = recovery_terminal::effect_source(tx, &record, ceiling)?;
    let lost = match evidence_record.receipt.disposition() {
        ReceiptDisposition::AlreadySettled => {
            // Unreachable: the settled marker was just checked. Refusing rather
            // than asserting keeps the impossible case honest if A0's
            // classification ever changes underneath this.
            return Err(WriteError::ReceiptMarkerOrder {
                attempt: attempt.as_str().to_owned(),
                detail: "the receipt classifies as settled without a settled marker".to_owned(),
            });
        }
        ReceiptDisposition::Lost(lost) => lost,
    };
    // Built FALLIBLY, because `AmbiguityEvidence` refuses an empty string and a
    // closure handed to A0's projection cannot refuse anything. The witness
    // `the_recorded_effect_matches_a0s_own_projection` pins this against
    // `LostEffect::into_effect_state` so the two can never drift into two rules.
    let effect = match lost {
        LostEffect::NoneInitiated => ExternalEffectState::NoneInitiated,
        LostEffect::Ambiguous { sent_at } => ExternalEffectState::Ambiguous {
            evidence: grapt_types::AmbiguityEvidence::new(
                evidence_record.receipt.describe_ambiguity(sent_at),
            )
            .ok_or_else(|| WriteError::ReceiptTerminalUnproven {
                attempt: attempt.as_str().to_owned(),
                terminal: record.terminal.event.as_str().to_owned(),
                detail: "the ambiguity description was empty, so no evidence could be recorded"
                    .to_owned(),
            })?,
        },
    };

    let fact = grapt_event::session_step_failed::StepFailedFact::new(
        record.receipt.step(),
        grapt_types::StepTerminalReason::ProcessLoss { effect },
    )
    .map_err(|error| WriteError::ReceiptTerminalUnproven {
        attempt: attempt.as_str().to_owned(),
        terminal: record.terminal.event.as_str().to_owned(),
        detail: format!("the process-loss terminal could not be constructed: {error}"),
    })?;

    // CLAIM-2: the streamed text of the block still open precedes its terminal.
    partial_text::promote(tx, &record, current, projectors)?;

    // The LANDED R2b-3 governed path, not a second event writer. An exact
    // same-owner retry of the identical fact reconciles as `Idempotent` there,
    // which is what makes a re-run of this whole function safe.
    crate::step_failed::record(
        tx,
        record.receipt.session(),
        &record.terminal.event,
        current,
        record.terminal.created_ms,
        &fact,
        projectors,
    )?;

    // Same transaction. Commit failure leaves NEITHER durable.
    write_settled_marker(tx, &record, settled_at)?;
    Ok(AttemptSettlement::Settled)
}

/// Writes the settled marker for a recovery settlement.
///
/// Deliberately does NOT re-check the reserve epoch: the caller has already
/// passed the election fence and the lineage check, and demanding equality here
/// is exactly the defect that made a crashed attempt unsettleable. This stays
/// crate-private and is called only after either the recovery route has appended
/// its terminal or the ordinary route has proven its already-durable terminal.
fn write_settled_marker(
    tx: &GovernedTransaction<'_>,
    record: &AttemptReceiptRecord,
    settled_at: SettledMarker,
) -> Result<(), WriteError> {
    let at = settled_at.at();
    let (updated, outcome) = record.receipt.clone().mark_settled(settled_at);
    if !outcome.changed() {
        return Ok(());
    }
    let updated = AttemptReceiptRecord::new(updated, record.writer_epoch, record.terminal.clone());
    verify_marker_order(&updated)?;
    let bytes = encode_record(&updated);
    tx.gate().execute(
        tx,
        GovernedTable::AttemptReceipt,
        "UPDATE attempt_receipt SET settled_ms = ?2, record_bytes = ?3 WHERE attempt_id = ?1",
        rusqlite::params![updated.receipt.attempt().as_str(), at.as_millis(), bytes],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Retention
// ---------------------------------------------------------------------------

/// Removes one settled receipt, but only after its terminal Step fact is read
/// back INSIDE this transaction.
///
/// # The ordering is the whole guarantee
///
/// The rule: "a receipt may be removed only atomically with, or strictly
/// after read-back proof of, the durable terminal Step fact; never before."
/// Both legal paths are this one function:
///
/// * **atomic** — the caller appends the terminal in this transaction and calls
///   this immediately; the read-back sees the uncommitted terminal and the
///   delete commits or rolls back WITH it;
/// * **reclaim** — a later pass, in a later PROCESS, reads the persisted
///   terminal coordinates out of the row; the read-back is a precondition inside
///   the transaction, not a check performed earlier.
///
/// One primitive rather than two is a deliberate simplification of the
/// preflight's two-path prose: a separate "atomic" entry point that skipped the
/// read-back would be a second deletion route with a weaker precondition, and
/// the rule exists precisely to forbid a delete whose proof was obtained
/// somewhere else.
///
/// # The RECEIPT names the terminal, not the caller
///
/// The exact `EventId` and `created_ms` were persisted at reserve, so this reads
/// them out of the row rather than accepting them from a caller. R2C5I finding
/// 3: a fresh process recovering after a crash has never seen that identity and
/// could not supply it, and a caller that COULD supply one could supply the
/// wrong one. The store then verifies the named event is a Step terminal,
/// belongs to THIS receipt's session, carries the persisted `created_ms`, and
/// settles THIS receipt's step. A scan for "some terminal on this session" would
/// let another Step's terminal authorise this removal, and would be unbounded
/// besides.
///
/// # Idempotence
///
/// An already-absent row is [`AttemptReceiptReclaimed::AlreadyAbsent`], not an
/// error: a pass that crashed after its delete committed must be able to re-run.
/// An UNSETTLED row is [`WriteError::ReceiptUnsettledDelete`] — never removed,
/// because an unsettled receipt is the only evidence that an external call may
/// be outstanding.
pub(crate) fn reclaim_within(
    tx: &GovernedTransaction<'_>,
    attempt: &PhysicalAttemptId,
    writer: FencedWriter,
    ceiling: AttemptRecordCeiling,
) -> Result<AttemptReceiptReclaimed, WriteError> {
    let stored = match read_row(tx, attempt.as_str())? {
        Some(stored) => stored,
        None => return Ok(AttemptReceiptReclaimed::AlreadyAbsent),
    };
    let record = decode_row(&stored, ceiling)?;
    fence_against_election(tx, record.receipt.session(), writer)?;
    // NOT equality, and this is the one place the two epoch laws differ.
    //
    // A marker records what its own owner did, so it demands the exact
    // reserving epoch. A reclaim is RECONCILIATION: it runs in a later process
    // that has taken the aggregate and therefore holds a NEWER epoch by
    // construction — that is the whole situation this lane exists for. Demanding
    // equality here would make every crashed attempt permanently unreclaimable,
    // which is the defect, not the law. What must still be refused is a STALE
    // reclaimer: an owner older than the row it wants to delete.
    if writer.epoch() < record.writer_epoch {
        return Err(WriteError::ReceiptStaleEpoch {
            attempt: attempt.as_str().to_owned(),
            held: record.writer_epoch.value(),
            offered: writer.epoch().value(),
        });
    }
    if record.receipt.settled_at().is_none() {
        return Err(WriteError::ReceiptUnsettledDelete {
            attempt: attempt.as_str().to_owned(),
        });
    }
    // The read-back, on THIS transaction. Its result is the delete's
    // precondition; there is no route to the database from here that is not
    // this transaction.
    prove_terminal_within(tx, &record)?;
    tx.gate().execute(
        tx,
        GovernedTable::AttemptReceipt,
        // Still conditional on `settled_ms IS NOT NULL` in the statement itself.
        // The Rust check above decides WHICH refusal to report; this clause is
        // what makes an unsettled row unremovable by this statement at all.
        "DELETE FROM attempt_receipt WHERE attempt_id = ?1 AND settled_ms IS NOT NULL",
        [attempt.as_str()],
    )?;
    Ok(AttemptReceiptReclaimed::Reclaimed)
}

/// Settles one receipt from an ALREADY DURABLE ordinary Step terminal, then
/// reclaims it in the caller's one governed transaction.
///
/// The terminal is an input fact, not something this seam manufactures. The
/// receipt names the exact `EventId`, created instant, session, and Step; the
/// same read-back used by retention proves every binding before the receipt's
/// settled marker is touched. Only then does the seam fill the marker and issue
/// the existing settled-guard delete. A crash or rollback therefore leaves the
/// terminal alone and the receipt unsettled; a committed call leaves the
/// terminal exactly once and the receipt gone.
pub(crate) fn settle_ordinary_terminal_within(
    tx: &GovernedTransaction<'_>,
    attempt: &PhysicalAttemptId,
    writer: FencedWriter,
    ceiling: AttemptRecordCeiling,
    settled_at: SettledMarker,
) -> Result<AttemptReceiptReclaimed, WriteError> {
    let Some(stored) = read_row(tx, attempt.as_str())? else {
        return Ok(AttemptReceiptReclaimed::AlreadyAbsent);
    };
    let record = decode_row(&stored, ceiling)?;
    fence_against_election(tx, record.receipt.session(), writer)?;
    if writer.epoch() < record.writer_epoch {
        return Err(WriteError::ReceiptStaleEpoch {
            attempt: attempt.as_str().to_owned(),
            held: record.writer_epoch.value(),
            offered: writer.epoch().value(),
        });
    }
    prove_terminal_within(tx, &record)?;
    write_settled_marker(tx, &record, settled_at)?;
    tx.gate().execute(
        tx,
        GovernedTable::AttemptReceipt,
        // Structural defence in depth: the marker was written immediately
        // above in this same transaction, so no production witness can
        // independently distinguish removal of this SQL predicate. LTS01
        // attributes the observable terminal-proof guard instead.
        "DELETE FROM attempt_receipt WHERE attempt_id = ?1 AND settled_ms IS NOT NULL",
        [attempt.as_str()],
    )?;
    Ok(AttemptReceiptReclaimed::Reclaimed)
}

/// Reads back the named terminal Step fact and proves it is THIS receipt's.
///
/// Three bindings, all required: the event is a Step terminal by durable name,
/// its aggregate is this receipt's session, and its decoded payload names this
/// receipt's step. Dropping any one of them would let a different fact authorise
/// this receipt's removal.
fn prove_terminal_within(
    conn: &rusqlite::Connection,
    record: &AttemptReceiptRecord,
) -> Result<(), WriteError> {
    let terminal = &record.terminal.event;
    let attempt = record.receipt.attempt().as_str().to_owned();
    let unproven = |detail: String| WriteError::ReceiptTerminalUnproven {
        attempt: attempt.clone(),
        terminal: terminal.as_str().to_owned(),
        detail,
    };
    let found: Option<(String, String, String, Option<String>, i64)> = conn
        .query_row(
            "SELECT aggregate_family, aggregate_id, type, data, created FROM event WHERE id = ?1",
            [terminal.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .map(Some)
        .or_else(|e| {
            if e == rusqlite::Error::QueryReturnedNoRows {
                Ok(None)
            } else {
                Err(WriteError::from(e))
            }
        })?;
    let Some((family, aggregate, kind, data, created)) = found else {
        return Err(unproven(
            "no such durable event in this transaction".to_owned(),
        ));
    };
    if family != SESSION_FAMILY {
        return Err(unproven(format!(
            "the named event belongs to the {family} family, not {SESSION_FAMILY}"
        )));
    }
    if aggregate != record.receipt.session().as_str() {
        return Err(unproven(format!(
            "the named event belongs to session {aggregate:?}, not the receipt's {:?}",
            record.receipt.session().as_str()
        )));
    }
    // The roster of durable Step terminals and the codec each one is read by
    // are ONE decision, taken here, once.
    //
    // Until L4b1 they were two: a name list, and a single hard-coded
    // `session_step_failed::decode` below it. That is the same law written in
    // two places, and it fails in the direction that hides itself — adding
    // `session.step.ended.2` to the list would have admitted the NAME and then
    // rejected its `{step, completion}` payload under the failure contract, so
    // the refusal moves from "is not a durable Step terminal fact" to "does not
    // decode" and no success receipt is proven. Every existing witness drives
    // the failure terminal, so nothing would have gone red.
    //
    // Selecting the decoder by kind makes the roster unable to grow without
    // naming the contract that reads it: a third terminal cannot be recognized
    // by adding a string. The match is closed and no arm accepts a terminal it
    // cannot decode.
    let decode_step: fn(&serde_json::Value) -> Result<grapt_types::StepNumber, String> =
        match kind.as_str() {
            STEP_FAILED_TERMINAL => |value| {
                grapt_event::session_step_failed::decode(value)
                    .map(|fact| fact.step())
                    .map_err(|error| error.to_string())
            },
            STEP_ENDED_TERMINAL => |value| {
                grapt_event::session_step_ended::decode(value)
                    .map(|fact| fact.step())
                    .map_err(|error| error.to_string())
            },
            "session.step.ended.3" => |value| {
                grapt_event::session_step_observed::decode_ended(value)
                    .map(|observed| observed.fact().step())
                    .map_err(|error| error.to_string())
            },
            STEP_FAILED_TERMINAL_V3 => |value| {
                grapt_event::session_step_observed::decode_failed(value)
                    .map(|observed| observed.fact().step())
                    .map_err(|error| error.to_string())
            },
            other => {
                return Err(unproven(format!(
                    "{other:?} is not a durable Step terminal fact"
                )))
            }
        };
    let Some(data) = data else {
        return Err(unproven(
            "the named Step terminal carries no payload".to_owned(),
        ));
    };
    let value: serde_json::Value = serde_json::from_str(&data)
        .map_err(|error| unproven(format!("the terminal payload is not JSON: {error}")))?;
    // Decoded by the contract the KIND names, so a payload of the other shape
    // refuses here rather than being read by a codec that was not written for
    // it. This is the binding a name-only roster could not make.
    let step = decode_step(&value)
        .map_err(|error| unproven(format!("the terminal payload does not decode: {error}")))?;
    if created != record.terminal.created_ms {
        return Err(unproven(format!(
            "the named terminal was created at {created}, not the reserved instant {}",
            record.terminal.created_ms
        )));
    }
    if step != record.receipt.step() {
        return Err(unproven(format!(
            "the named terminal settles step {}, not the receipt's step {}",
            step.value(),
            record.receipt.step().value()
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// Reads one receipt by exact attempt id.
pub(crate) fn read_one(
    conn: &rusqlite::Connection,
    attempt: &PhysicalAttemptId,
    ceiling: AttemptRecordCeiling,
) -> Result<Option<AttemptReceiptRecord>, WriteError> {
    let Some(stored) = read_row(conn, attempt.as_str())? else {
        return Ok(None);
    };
    decode_row(&stored, ceiling).map(Some)
}

/// Reads the NEXT row a bounded recovery pass must act on, one row at a time.
///
/// # Why one row, and not a page
///
/// R2C5I ruling 6. A `Vec` page bounded by a row count and a per-row ceiling is
/// not a memory proof — it is `page x ceiling` live at once, and the row count
/// was never the bound that mattered. This returns ONE row, after comparing that
/// row's DECLARED size against the budget the caller has left, so the refusal
/// happens before the allocation rather than after it.
///
/// `remaining_bytes` is REQUIRED and injected. This lane owns the one-row
/// primitive and the per-row decision; B owns the cumulative per-pass budget and
/// passes what remains of it. Neither number is chosen here, and there is no
/// `Default`.
///
/// # The scan includes SETTLED rows
///
/// R2C5I ruling 4. A settled receipt whose reclaim never ran is a leak, and a
/// scan that filtered to unsettled rows could never see it — which is exactly
/// how an after-terminal/before-reclaim death became invisible. Enumeration is
/// ordered by attempt id alone and the caller decides what each row needs.
///
/// # What this does NOT do
///
/// It never quarantines, settles, reclaims or advances anything durable. It
/// reads. The cursor it hands back is advisory until the caller commits the
/// action that row demands.
pub(crate) fn next_recovery_row(
    conn: &rusqlite::Connection,
    session: &SessionId,
    after: Option<&PhysicalAttemptId>,
    remaining_bytes: usize,
    ceiling: AttemptRecordCeiling,
) -> Result<RecoveryStep, WriteError> {
    let floor = after.map(|id| id.as_str().to_owned()).unwrap_or_default();
    // ONE statement decides the size AND yields the blob, so the admission and
    // the materialization see the same row.
    //
    // An earlier revision asked `length(record_bytes)` in one statement, checked
    // the bounds, then re-read the row in a SECOND statement. Nothing held the
    // two together — this takes a bare `&Connection`, not a transaction — so a
    // concurrent writer could grow the row in between and the second read would
    // allocate past the bound that had just been approved. The give-away was the
    // vanished-row branch that revision needed: a row can only vanish between
    // two reads if there are two reads.
    //
    // The CASE is what keeps the guarantee. `length()` is evaluated on the same
    // row visit, and the blob is produced ONLY when it is within both the
    // per-row ceiling and this pass's remaining budget; otherwise the column
    // comes back NULL and the bytes are never handed to this process. A single
    // statement also cannot observe a vanish mid-row, so that branch is gone.
    // EVERY column the decode compares against comes from this one row visit
    // too. Re-reading the identity columns separately would have left exactly
    // the same race one column to the left: the column↔bytes grid only means
    // something if both sides were observed together.
    let limit = i64::try_from(ceiling.as_bytes().min(remaining_bytes)).unwrap_or(i64::MAX);
    let found: Option<(StoredRow, i64, Option<Vec<u8>>)> = conn
        .query_row(
            "SELECT attempt_id, session_id, step, ordinal, writer_epoch,
                    terminal_event_id, terminal_created_ms,
                    reserved_ms, request_sent_ms, settled_ms,
                    length(record_bytes),
                    CASE WHEN length(record_bytes) <= ?3 THEN record_bytes ELSE NULL END
               FROM attempt_receipt
              WHERE session_id = ?1 AND attempt_id > ?2
              ORDER BY attempt_id ASC LIMIT 1",
            rusqlite::params![session.as_str(), floor, limit],
            |r| {
                Ok((
                    StoredRow {
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
                        // Filled from column 11 once the bounds admit it.
                        record_bytes: Vec::new(),
                    },
                    r.get(10)?,
                    r.get(11)?,
                ))
            },
        )
        .map(Some)
        .or_else(|e| {
            if e == rusqlite::Error::QueryReturnedNoRows {
                Ok(None)
            } else {
                Err(WriteError::from(e))
            }
        })?;

    let Some((columns, declared, blob)) = found else {
        return Ok(RecoveryStep::Exhausted);
    };
    let attempt_id = columns.attempt_id.clone();
    let declared = usize::try_from(declared).unwrap_or(usize::MAX);
    let attempt = PhysicalAttemptId::new(attempt_id.clone()).ok_or_else(|| {
        WriteError::ReceiptRecordMismatch {
            attempt: attempt_id.clone(),
            detail: "the row's primary key is empty, which is not an attempt identity".to_owned(),
        }
    })?;

    // Over the per-row ceiling: refused from its declared size, never fetched,
    // and handed back as durable-quarantine work rather than skipped.
    if declared > ceiling.as_bytes() {
        return Ok(RecoveryStep::Row {
            row: RecoveryRow::Untrustworthy {
                attempt: attempt.clone(),
                declared_bytes: declared,
                reason: QuarantineReason::OverCeiling,
            },
            next_after: attempt,
            bytes_charged: 0,
        });
    }
    // Within the ceiling but beyond what this pass has left. The cursor does
    // NOT move and nothing is fetched.
    if declared > remaining_bytes {
        return Ok(RecoveryStep::BudgetExhausted {
            declared_bytes: declared,
            remaining: remaining_bytes,
        });
    }

    // The blob came back with the size decision, from the same row visit. A
    // NULL here after both bounds have already been cleared would mean the CASE
    // and the two comparisons above disagreed about the same `length()`, which
    // is a contradiction rather than a race — so it refuses rather than falling
    // back to a second read that would reintroduce the very gap this closed.
    let Some(record_bytes) = blob else {
        return Err(WriteError::ReceiptRecordMismatch {
            attempt: attempt_id.clone(),
            detail: format!(
                "the bounded read withheld a {declared}-byte record that passed both the \
                 {} ceiling and the {remaining_bytes} remaining budget",
                ceiling.as_bytes()
            ),
        });
    };
    let stored = StoredRow {
        record_bytes,
        ..columns
    };
    let row = match decode_row(&stored, ceiling) {
        Ok(record) => RecoveryRow::Receipt(record),
        // A row this decoder cannot read is durable-quarantine work too, and
        // reporting it is what stops a pass from silently narrowing around it.
        Err(WriteError::ReceiptUndecodable { .. })
        | Err(WriteError::ReceiptRecordMismatch { .. }) => RecoveryRow::Untrustworthy {
            attempt: attempt.clone(),
            declared_bytes: declared,
            reason: QuarantineReason::Undecodable,
        },
        Err(other) => return Err(other),
    };
    Ok(RecoveryStep::Row {
        row,
        next_after: attempt,
        bytes_charged: stored.record_bytes.len(),
    })
}
