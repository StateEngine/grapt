//! Session-bound selections for conversation rewind. A selection captures an
//! exact aggregate tip; the eventual write must compare it under its transaction.
// Copyright 2025 OpenAI. Licensed under the Apache License, Version 2.0.
// Modified by Grapt:
// retain canonical events and apply durable, bounded visibility intervals.

use crate::{SessionStore, SessionStoreError};
use grapt_store::history_visibility::{HistoryVisibility, VisibilityError};
use grapt_store::{AggregateId, AppendInput, Appended, EventStore, Projector, WriteError};
use grapt_types::{DeterministicAuthorities, EventId, EventVersion};
use grapt_types::{EventSeq, MessageId, SessionId};
use rusqlite::OptionalExtension;
use serde_json::Value;

const MAX_PROMPT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewindSelection {
    session: SessionId,
    before: MessageId,
    boundary: EventSeq,
    tip: EventSeq,
    prompt: String,
}

impl RewindSelection {
    pub fn session(&self) -> &SessionId {
        &self.session
    }

    pub fn before(&self) -> &MessageId {
        &self.before
    }

    pub fn boundary(&self) -> EventSeq {
        self.boundary
    }

    pub fn tip(&self) -> EventSeq {
        self.tip
    }

    pub fn prompt(&self) -> &str {
        &self.prompt
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RewindError {
    #[error(transparent)]
    Session(#[from] SessionStoreError),
    #[error(transparent)]
    Visibility(#[from] VisibilityError),
    #[error(transparent)]
    Write(#[from] WriteError),
    #[error("the session has pending input or compaction; settle it before rewinding")]
    Pending,
    #[error("the selected prompt steered an execution; rewind to that execution’s initial prompt")]
    MidExecution,
    #[error("rewind event identity is invalid or repeated")]
    Identity,
    #[error("rewind accepts 1–200 earlier prompts and searches at most 200 surviving inputs")]
    SelectionBound,
    #[error("rewind selection could not be read: {0}")]
    Read(#[from] rusqlite::Error),
    #[error("the selected message is not a visible user prompt in this session")]
    NotUserPrompt,
    #[error("rewind session and message identities must fit within 512 bytes")]
    AddressTooLong,
    #[error("the selected prompt exceeds the 1 MiB rewind limit")]
    PromptTooLong,
    #[error("rewind history is corrupt: {0}")]
    Corrupt(&'static str),
    #[error("the session changed after the rewind selection; select the prompt again")]
    Stale,
}

/// Select an initial user prompt counted backwards from the newest visible
/// history. Mid-execution steers are not independent editing boundaries.
pub fn previous(
    store: &EventStore,
    session: &SessionId,
    prompts: usize,
) -> Result<RewindSelection, RewindError> {
    if !(1..=200).contains(&prompts) {
        return Err(RewindError::SelectionBound);
    }
    SessionStore::over(store).record(session)?;
    let conn = store.connection();
    let tip = aggregate_tip(conn, session)?;
    let visible = HistoryVisibility::read(conn, session)?.sql("seq");
    let mut statement = conn.prepare(&format!(
        "SELECT CASE WHEN length(CAST(id AS BLOB)) <= 512 THEN id END, type
         FROM session_message WHERE session_id = ?1 AND {visible}
         ORDER BY seq DESC LIMIT 201"
    ))?;
    let mut rows = statement.query([session.as_str()])?;
    let mut found = 0;
    let mut scanned = 0;
    while let Some(row) = rows.next()? {
        if scanned == 200 {
            return Err(RewindError::SelectionBound);
        }
        scanned += 1;
        let kind: String = row.get(1)?;
        match kind.as_str() {
            "user" => {}
            "synthetic" | "assistant" | "system" | "compaction" => continue,
            _ => {
                return Err(RewindError::Corrupt(
                    "the projected message kind is unknown",
                ))
            }
        }
        let raw: Option<String> = row.get(0)?;
        let input: MessageId = raw
            .ok_or(RewindError::AddressTooLong)?
            .parse()
            .map_err(|_| RewindError::Corrupt("projected prompt identity is invalid"))?;
        let selection = match select(store, session, &input) {
            Err(RewindError::MidExecution) => continue,
            result => result?,
        };
        if selection.tip != tip {
            return Err(RewindError::Stale);
        }
        found += 1;
        if found == prompts {
            return Ok(selection);
        }
    }
    Err(RewindError::NotUserPrompt)
}

/// Capture a user-visible prompt without holding a database transaction across
/// interaction. Matching tips around the reads establish one immutable source;
/// append-only aggregate sequences cannot return to an earlier tip.
pub fn select(
    store: &EventStore,
    session: &SessionId,
    before: &MessageId,
) -> Result<RewindSelection, RewindError> {
    select_connection(store.connection(), session, before, true)
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn select_connection(
    connection: &rusqlite::Connection,
    session: &SessionId,
    before: &MessageId,
    require_visible: bool,
) -> Result<RewindSelection, RewindError> {
    if session.as_str().len() > 512 || before.as_str().len() > 512 {
        return Err(RewindError::AddressTooLong);
    }
    SessionStore::new(connection).record(session)?;
    let tip = aggregate_tip(connection, session)?;
    let row: Option<(String, i64, i64, Option<String>)> = connection
        .query_row(
            "SELECT type, seq, length(CAST(text AS BLOB)),
             CASE WHEN length(CAST(text AS BLOB)) <= 1048576 THEN text END
             FROM session_message WHERE session_id = ?1 AND id = ?2",
            rusqlite::params![session.as_str(), before.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((kind, raw_boundary, length, prompt)) = row else {
        return Err(RewindError::NotUserPrompt);
    };
    match kind.as_str() {
        "user" => {}
        "synthetic" | "assistant" | "system" | "compaction" => {
            return Err(RewindError::NotUserPrompt)
        }
        _ => {
            return Err(RewindError::Corrupt(
                "the projected message kind is unknown",
            ))
        }
    }
    if length > MAX_PROMPT_BYTES as i64 {
        return Err(RewindError::PromptTooLong);
    }
    let prompt = prompt.ok_or(RewindError::Corrupt("the selected prompt text is absent"))?;
    let boundary = sequence(raw_boundary)?;
    let preceding: Option<String> = connection
        .query_row(
            "SELECT type FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
         AND seq < ?2 AND type LIKE 'session.execution.%' ORDER BY seq DESC LIMIT 1",
            (session.as_str(), raw_boundary),
            |row| row.get(0),
        )
        .optional()?;
    match preceding.as_deref() {
        Some("session.execution.started.1") => {
            // A prior process may have stopped after recording its start. An
            // observation is history, not live ownership: this input's own
            // subsequent start still identifies it as an initial prompt.
            let following: Option<String> = connection.query_row(
                "SELECT type FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
                 AND seq > ?2 AND type LIKE 'session.execution.%'
                 AND seq < COALESCE((SELECT MIN(seq) FROM event WHERE aggregate_family = 'session'
                     AND aggregate_id = ?1 AND seq > ?2 AND type = 'session.input.promoted.1'), 9223372036854775807)
                 ORDER BY seq LIMIT 1",
                (session.as_str(), raw_boundary), |row| row.get(0),
            ).optional()?;
            match following.as_deref() {
                Some("session.execution.started.1") => {}
                Some(
                    "session.execution.succeeded.1"
                    | "session.execution.failed.1"
                    | "session.execution.interrupted.1",
                )
                | None => return Err(RewindError::MidExecution),
                Some(_) => {
                    return Err(RewindError::Corrupt(
                        "the following execution observation is unsupported",
                    ))
                }
            }
        }
        Some(
            "session.execution.succeeded.1"
            | "session.execution.failed.1"
            | "session.execution.interrupted.1",
        )
        | None => {}
        Some(_) => {
            return Err(RewindError::Corrupt(
                "the preceding execution observation is unsupported",
            ))
        }
    }

    if require_visible && !HistoryVisibility::read(connection, session)?.contains(boundary.value())
    {
        return Err(RewindError::NotUserPrompt);
    }
    if boundary > tip {
        return Err(RewindError::Corrupt(
            "the selected prompt is beyond the session tip",
        ));
    }
    let source: Option<(String, Option<String>)> = connection
        .query_row(
            "SELECT type, CASE WHEN length(CAST(data AS BLOB)) <= 1048576 THEN data END
             FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1 AND seq = ?2",
            rusqlite::params![session.as_str(), raw_boundary],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((kind, data)) = source else {
        return Err(RewindError::Corrupt(
            "the selected prompt has no promotion event",
        ));
    };
    let data = data.ok_or(RewindError::Corrupt("the promotion event exceeds 1 MiB"))?;
    let data: Value = serde_json::from_str(&data)
        .map_err(|_| RewindError::Corrupt("the promotion event is not JSON"))?;
    if kind != "session.input.promoted.1"
        || data.get("sessionID").and_then(Value::as_str) != Some(session.as_str())
        || data.get("inputID").and_then(Value::as_str) != Some(before.as_str())
    {
        return Err(RewindError::Corrupt(
            "the selected prompt has a different promotion",
        ));
    }
    corroborate_prompt(connection, session, before, &prompt)?;
    if aggregate_tip(connection, session)? != tip {
        return Err(RewindError::Stale);
    }
    Ok(RewindSelection {
        session: session.clone(),
        before: before.clone(),
        boundary,
        tip,
        prompt,
    })
}

/// Reconstruct an exact preview for commit/reconciliation. A hidden target is
/// usable only by the deterministic operation identity of an already committed
/// request; a new request still requires a visible prompt and an unchanged tip.
pub fn for_commit(
    store: &EventStore,
    session: &SessionId,
    before: &MessageId,
    expected_tip: EventSeq,
) -> Result<RewindSelection, RewindError> {
    let mut selection = select_connection(store.connection(), session, before, false)?;
    if selection.tip == expected_tip {
        return select(store, session, before);
    }
    selection.tip = expected_tip;
    let event = operation_id(&selection)?;
    let exists: bool = store.connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM event WHERE id = ?1)",
        [event.as_str()],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(RewindError::Stale);
    }
    Ok(selection)
}

fn operation_id(selection: &RewindSelection) -> Result<EventId, RewindError> {
    let digest = grapt_event::canonical_hash_hex(&serde_json::json!({
        "sessionID":selection.session.as_str(), "to":selection.before.as_str(), "tip":selection.tip.value(),
    }));
    format!("evt_rewind_{digest}")
        .parse()
        .map_err(|_| RewindError::Identity)
}

/// Commit one cutoff after validating the exact source under BEGIN IMMEDIATE.
/// The caller supplies existing instruction/projector owners; every projection
/// and the durable marker commit together. No filesystem effect occurs here.
pub fn commit(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    selection: &RewindSelection,
    projectors: &mut [&mut dyn Projector],
) -> Result<u64, RewindError> {
    let aggregate = AggregateId::Session(selection.session.clone());
    store.elect_writer(&aggregate)?;
    let writer = store.fencing_token(&aggregate)?;
    let tx = store.governed_transaction().map_err(WriteError::from)?;
    let event = operation_id(selection)?;
    let existing: Option<CommittedCut> = tx
        .query_row(
            "SELECT aggregate_family, aggregate_id, seq, created, type,
             CASE WHEN length(CAST(data AS BLOB)) <= 4096 THEN data END, writer_epoch
             FROM event WHERE id = ?1",
            [event.as_str()],
            |row| {
                Ok(CommittedCut {
                    family: row.get(0)?,
                    session: row.get(1)?,
                    seq: row.get(2)?,
                    created: row.get(3)?,
                    kind: row.get(4)?,
                    data: row.get(5)?,
                    writer_epoch: row.get(6)?,
                })
            },
        )
        .optional()?;
    let fact = grapt_event::session_revert::RevertCommittedFact::new(
        selection.session.clone(),
        selection.before.clone(),
    );
    let version = EventVersion::new(1).ok_or(RewindError::Identity)?;
    let data = grapt_event::session_revert::encode(&fact);
    if let Some(existing) = existing {
        // This reconciles the original owner's fact. Re-appending under a new
        // epoch would correctly diverge at the store's full-identity boundary.
        return existing.reconcile(&event, selection, &fact);
    }
    if aggregate_tip(&tx, &selection.session)? != selection.tip {
        return Err(RewindError::Stale);
    }
    let fresh = select_connection(&tx, &selection.session, &selection.before, true)?;
    if &fresh != selection {
        return Err(RewindError::Stale);
    }
    let pending: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM session_pending WHERE session_id = ?1)",
        [selection.session.as_str()],
        |row| row.get(0),
    )?;
    if pending {
        return Err(RewindError::Pending);
    }
    if HistoryVisibility::read(&tx, &selection.session)?.count()
        >= grapt_store::history_visibility::MAX_REWINDS
    {
        return Err(VisibilityError::TooLong.into());
    }
    let mut list = crate::SessionListProjector;
    let mut owners: Vec<&mut dyn Projector> = vec![&mut list];
    for owner in projectors {
        owners.push(&mut **owner);
    }
    let appended = EventStore::append_within(
        &tx,
        AppendInput {
            id: &event,
            aggregate,
            writer,
            kind_name: "session.revert.committed",
            version,
            created_ms: authorities.now().as_millis(),
            data: &data,
        },
        &mut owners,
    )?;
    let seq = match appended {
        Appended::Committed { seq } => seq,
        Appended::Idempotent { .. } => return Err(RewindError::Identity),
    };
    tx.commit().map_err(WriteError::from)?.discard();
    Ok(seq)
}

struct CommittedCut {
    family: String,
    session: String,
    seq: i64,
    created: i64,
    kind: String,
    data: Option<String>,
    writer_epoch: Option<i64>,
}

impl CommittedCut {
    fn reconcile(
        self,
        id: &EventId,
        selection: &RewindSelection,
        fact: &grapt_event::session_revert::RevertCommittedFact,
    ) -> Result<u64, RewindError> {
        let raw = self
            .data
            .ok_or(RewindError::Corrupt("rewind receipt exceeds 4096 bytes"))?;
        let record = grapt_event::decode_for_replay(
            id.as_str(),
            &self.session,
            self.seq,
            self.created,
            &self.kind,
            &raw,
            grapt_event::LegacyDataLimit::MEASURED_DEFAULT,
        )
        .map_err(|_| RewindError::Corrupt("the committed rewind receipt is invalid"))?;
        let original_epoch = self
            .writer_epoch
            .and_then(|epoch| u32::try_from(epoch).ok());
        if self.family != "session"
            || record.aggregate() != &selection.session
            || record.kind() != grapt_event::EventKind::SessionRevertCommittedV1
            || !original_epoch.is_some_and(|epoch| epoch > 0)
            || grapt_event::session_revert::decode(record.data()).as_ref() != Ok(fact)
        {
            return Err(RewindError::Identity);
        }
        let seq = record.seq().value();
        if Some(seq) != selection.tip.value().checked_add(1) {
            return Err(RewindError::Stale);
        }
        Ok(seq)
    }
}

fn corroborate_prompt(
    conn: &rusqlite::Connection,
    session: &SessionId,
    input: &MessageId,
    prompt: &str,
) -> Result<(), RewindError> {
    let mut statement = conn.prepare(
        "SELECT CASE WHEN length(CAST(data AS BLOB)) <= 8388608 THEN data END
         FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
         AND type = 'session.input.admitted.1' AND CASE WHEN json_valid(data) = 0 THEN 1
           WHEN json_type(data, '$.inputID') IS NOT 'text' THEN 1
           ELSE json_extract(data, '$.inputID') = ?2 END ORDER BY seq LIMIT 2",
    )?;
    let mut rows = statement.query((session.as_str(), input.as_str()))?;
    let row = rows
        .next()?
        .ok_or(RewindError::Corrupt("selected prompt has no admission"))?;
    let raw: Option<String> = row.get(0)?;
    let raw = raw.ok_or(RewindError::Corrupt(
        "selected admission exceeds its read bound",
    ))?;
    let data: Value = serde_json::from_str(&raw)
        .map_err(|_| RewindError::Corrupt("selected admission is not JSON"))?;
    if data.get("sessionID").and_then(Value::as_str) != Some(session.as_str())
        || data.get("inputID").and_then(Value::as_str) != Some(input.as_str())
        || data.pointer("/input/type").and_then(Value::as_str) != Some("user")
        || data.pointer("/input/data/text").and_then(Value::as_str) != Some(prompt)
    {
        return Err(RewindError::Corrupt(
            "selected prompt differs from its canonical admission",
        ));
    }
    if rows.next()?.is_some() {
        return Err(RewindError::Corrupt(
            "selected prompt has duplicate admissions",
        ));
    }
    Ok(())
}

fn aggregate_tip(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<EventSeq, RewindError> {
    let tip: Option<i64> = connection.query_row(
        "SELECT MAX(seq) FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1",
        [session.as_str()],
        |row| row.get(0),
    )?;
    sequence(tip.ok_or(RewindError::Corrupt("the session has no durable events"))?)
}

fn sequence(raw: i64) -> Result<EventSeq, RewindError> {
    u64::try_from(raw)
        .map(EventSeq::new)
        .map_err(|_| RewindError::Corrupt("a history sequence is negative"))
}
