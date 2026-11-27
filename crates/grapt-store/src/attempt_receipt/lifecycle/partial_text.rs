//! CLAIM-2: keep a process-lost Step's streamed text.
//!
//! A text block's bytes are held in memory until it closes, and its owner
//! checkpoints them at a bounded cadence (`session.content.checkpointed.1`).
//! When recovery settles the Step as a process loss, the newest checkpoint of
//! the block that was still open becomes an ordinary text item of the Step's
//! message, appended in the SAME transaction as the terminal, so readers see
//! what a user interrupt leaves: a normal text item in a failed Step.
//!
//! # Which text, exactly
//!
//! Only the Step's active attempt: the newest of the receipts sharing this
//! terminal, chosen the same way whichever of them recovery settles first. A
//! retry keeps every earlier receipt until the terminal, and a block an
//! earlier attempt opened was discarded by that retry, so it stays discarded.
//! The active attempt's block must be the message's last text block opening,
//! and no text append may follow that opening: a block that closed already
//! has its own item and is never duplicated. Once the terminal exists, later
//! receipts and repeated recovery reuse it and promote nothing again.
//!
//! A stored row this module cannot read promotes nothing rather than refusing
//! the settlement: a missing partial is a smaller loss than a Step that can
//! never settle. A storage error still refuses, as everywhere in recovery.
//!
//! The item's identity is derived from the terminal's reserved identity, and
//! its time is the checkpoint's own, so recovery still mints nothing.

use super::*;
use grapt_event::session_message::{self, checkpoint, ContentAppendedFact};
use grapt_types::{
    CaptureDisposition, ContentKind, ContentOrdinal, EventId, NormalizedContentItem,
};

/// The largest row payload read here. Every row this reads is a few hundred
/// bytes; a larger one is not a member of its contract.
const ROW_BYTES: i64 = 4096;

pub(super) fn promote(
    tx: &GovernedTransaction<'_>,
    record: &AttemptReceiptRecord,
    current: FencedWriter,
    projectors: &mut [&mut dyn Projector],
) -> Result<(), WriteError> {
    let Some((id, created_ms, fact)) = partial(tx, record)? else {
        return Ok(());
    };
    crate::session_message::record_appended(
        tx,
        record.receipt.session(),
        &id,
        current,
        created_ms,
        &fact,
        projectors,
    )
    .map(|_| ())
}

/// The text item to append, when there is one.
fn partial(
    tx: &GovernedTransaction<'_>,
    record: &AttemptReceiptRecord,
) -> Result<Option<(EventId, i64, ContentAppendedFact)>, WriteError> {
    let session = record.receipt.session().as_str();
    let step = i64::from(record.receipt.step().value());
    let Some(active) = active_attempt(tx, record)? else {
        return Ok(None);
    };
    let Some(message) = step_message(tx, session, step)? else {
        return Ok(None);
    };
    let Some((opened_seq, opening)) = last_text_opening(tx, session, &message)? else {
        return Ok(None);
    };
    if opening.attempt().as_str() != active {
        return Ok(None);
    }
    let closed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM event
         WHERE aggregate_family = 'session' AND aggregate_id = ?1
         AND type = 'session.content.appended.1' AND seq > ?2
         AND json_valid(data) AND json_extract(data, '$.message') = ?3
         AND json_extract(data, '$.item.kind') = 'text')",
        (session, opened_seq, message.as_str()),
        |row| row.get(0),
    )?;
    if closed {
        return Ok(None);
    }
    let newest: Option<(i64, String)> = optional(tx.query_row(
        "SELECT created, data FROM event
         WHERE aggregate_family = 'session' AND aggregate_id = ?1
         AND type = 'session.content.checkpointed.1' AND seq > ?2
         AND length(CAST(data AS BLOB)) <= ?6 AND json_valid(data)
         AND json_extract(data, '$.message') = ?3 AND json_extract(data, '$.attempt') = ?4
         AND json_extract(data, '$.block') = ?5
         ORDER BY seq DESC LIMIT 1",
        (
            session,
            opened_seq,
            message.as_str(),
            active.as_str(),
            i64::from(opening.block()),
            ROW_BYTES,
        ),
        |row| Ok((row.get(0)?, row.get(1)?)),
    ))?;
    let Some((created_ms, data)) = newest else {
        return Ok(None);
    };
    let Some(saved) = serde_json::from_str(&data)
        .ok()
        .and_then(|value| checkpoint::decode_checkpointed(&value).ok())
    else {
        return Ok(None);
    };
    let Some(ordinal) = next_ordinal(tx, session, &message)? else {
        return Ok(None);
    };
    let item = NormalizedContentItem::new(ordinal, ContentKind::Text, saved.text().clone());
    let Ok(fact) = ContentAppendedFact::new(message, item, CaptureDisposition::Complete) else {
        return Ok(None);
    };
    let Ok(id) = format!("{}_partial", record.terminal.event.as_str()).parse() else {
        return Ok(None);
    };
    Ok(Some((id, created_ms, fact)))
}

/// The Step's active attempt. A receipt's ordinal counts every retry and
/// summary call of its Step, so the highest ordinal among the receipts sharing
/// this terminal is the attempt that was running at the loss. A tie names no
/// single attempt, and promotes nothing.
fn active_attempt(
    tx: &GovernedTransaction<'_>,
    record: &AttemptReceiptRecord,
) -> Result<Option<String>, WriteError> {
    let (count, attempt): (i64, Option<String>) = tx.query_row(
        "SELECT COUNT(*), MIN(attempt_id) FROM attempt_receipt
         WHERE session_id = ?1 AND terminal_event_id = ?2
         AND ordinal = (SELECT MAX(ordinal) FROM attempt_receipt
                        WHERE session_id = ?1 AND terminal_event_id = ?2)",
        (
            record.receipt.session().as_str(),
            record.terminal.event.as_str(),
        ),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(attempt.filter(|_| count == 1))
}

/// The Step's assistant message.
fn step_message(
    tx: &GovernedTransaction<'_>,
    session: &str,
    step: i64,
) -> Result<Option<grapt_types::MessageId>, WriteError> {
    let data: Option<(String, String)> = optional(tx.query_row(
        "SELECT type, data FROM event
         WHERE aggregate_family = 'session' AND aggregate_id = ?1
         AND type IN ('session.assistant-message.opened.1', 'session.assistant-message.opened.2')
         AND length(CAST(data AS BLOB)) <= ?3 AND json_valid(data)
         AND json_extract(data, '$.step') = ?2
         ORDER BY seq DESC LIMIT 1",
        (session, step, ROW_BYTES),
        |row| Ok((row.get(0)?, row.get(1)?)),
    ))?;
    Ok(data.and_then(|(kind, data)| {
        let value = serde_json::from_str(&data).ok()?;
        let opened = if kind.ends_with(".2") {
            session_message::decode_opened_v2(&value)
        } else {
            session_message::decode_opened(&value)
        };
        opened.ok().map(|opened| opened.message().clone())
    }))
}

fn last_text_opening(
    tx: &GovernedTransaction<'_>,
    session: &str,
    message: &grapt_types::MessageId,
) -> Result<Option<(i64, session_message::ContentBlockOpenedFact)>, WriteError> {
    let row: Option<(i64, String)> = optional(tx.query_row(
        "SELECT seq, data FROM event
         WHERE aggregate_family = 'session' AND aggregate_id = ?1
         AND type = 'session.content-block.opened.1'
         AND length(CAST(data AS BLOB)) <= ?3 AND json_valid(data)
         AND json_extract(data, '$.message') = ?2 AND json_extract(data, '$.kind') = 'text'
         ORDER BY seq DESC LIMIT 1",
        (session, message.as_str(), ROW_BYTES),
        |row| Ok((row.get(0)?, row.get(1)?)),
    ))?;
    Ok(row.and_then(|(seq, data)| {
        let value = serde_json::from_str(&data).ok()?;
        session_message::decode_block_opened(&value)
            .ok()
            .map(|opening| (seq, opening))
    }))
}

/// The ordinal after the message's newest content item: ordinals are dense
/// and committed in order. `None` when that row cannot be read.
fn next_ordinal(
    tx: &GovernedTransaction<'_>,
    session: &str,
    message: &grapt_types::MessageId,
) -> Result<Option<ContentOrdinal>, WriteError> {
    // The newest row whatever its size: skipping an unreadable one would fall
    // back to an older ordinal and collide with it.
    let row: Option<Option<String>> = optional(tx.query_row(
        "SELECT CASE WHEN length(CAST(data AS BLOB)) <= ?3 THEN data END
         FROM event
         WHERE aggregate_family = 'session' AND aggregate_id = ?1
         AND type = 'session.content.appended.1'
         AND json_valid(data) AND json_extract(data, '$.message') = ?2
         ORDER BY seq DESC LIMIT 1",
        (session, message.as_str(), ROW_BYTES),
        |row| row.get(0),
    ))?;
    let Some(data) = row else {
        return Ok(Some(ContentOrdinal::FIRST));
    };
    Ok(data
        .and_then(|data| serde_json::from_str(&data).ok())
        .and_then(|value| session_message::decode_appended(&value).ok())
        .and_then(|newest| newest.item().ordinal().next()))
}

fn optional<T>(row: rusqlite::Result<T>) -> Result<Option<T>, WriteError> {
    match row {
        Ok(value) => Ok(Some(value)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error.into()),
    }
}
