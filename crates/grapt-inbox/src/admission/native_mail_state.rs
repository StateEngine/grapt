//! Durable native wake eligibility, read and changed under the input transaction.
use super::*;
use crate::event::native_mail::{decode_state, validate_state, NativeMailState};
use rusqlite::OptionalExtension;

/// Explicit host authority releases only the exact observed stop generation.
#[derive(Debug)]
pub struct NativeMailResume<'a> {
    pub session: &'a SessionId,
    pub hold: &'a EventId,
    pub target: &'a EventId,
    pub binding: &'a str,
    pub event: &'a EventId,
    pub created_ms: i64,
}

impl NativeMailState {
    fn available(&self) -> bool {
        !self.held && (self.target.is_none() || self.binding.is_some())
    }
}

fn latest(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<(Option<EventId>, NativeMailState), InboxError> {
    let row: Option<(String, String)> = connection
        .query_row(
            "SELECT CASE WHEN length(CAST(id AS BLOB))<=2048 THEN id END, CASE WHEN length(CAST(data AS BLOB))<=4096 THEN data END
         FROM event WHERE aggregate_family='session' AND aggregate_id=?1
           AND type='session.native-mail-state.1' ORDER BY seq DESC LIMIT 1",
            [session.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((id, raw)) = row else {
        return Ok((
            None,
            NativeMailState {
                session_id: session.clone(),
                held: false,
                human_resumed: false,
                target: None,
                binding: None,
            },
        ));
    };
    let value: Value = serde_json::from_str(&raw)
        .map_err(|_| InboxError::PendingBound("native mail state encoding"))?;
    let state = decode_state(&value).map_err(InboxError::PendingBound)?;
    if state.session_id != *session {
        return Err(InboxError::PendingBound("native mail state session"));
    }
    Ok((
        Some(
            id.parse()
                .map_err(|_| InboxError::PendingBound("native mail state identity"))?,
        ),
        state,
    ))
}

pub(super) fn human_resumed(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<bool, InboxError> {
    Ok(latest(connection, session)?.1.human_resumed)
}

fn binding_matches(
    connection: &rusqlite::Connection,
    session: &SessionId,
    binding: &str,
) -> Result<bool, InboxError> {
    let raw: Option<String> = connection
        .query_row(
            "SELECT CASE WHEN length(CAST(value AS BLOB))<=65536 THEN value END
         FROM instruction_entry WHERE session_id=?1 AND key='stateengine-identity' AND removed=0",
            [session.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    let Some(raw) = raw else {
        return Ok(false);
    };
    let expectation = grapt_store::MailBindingExpectation::new(
        grapt_types::InstructionKey::new("stateengine-identity")
            .ok_or(InboxError::PendingBound("native mail binding key"))?,
        raw,
    )?;
    Ok(expectation.fingerprint() == binding)
}

pub(super) fn notice_target(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Option<(EventId, String, bool)>, InboxError> {
    let (_, state) = latest(connection, session)?;
    let (Some(target), Some(binding)) = (state.target, state.binding) else {
        return Ok(None);
    };
    if !binding_matches(connection, session, &binding)? {
        return Err(InboxError::PendingBound(
            "native mail target binding changed",
        ));
    }
    Ok(Some((target, binding, state.held)))
}

fn append(
    tx: &GovernedTransaction<'_>,
    held: &HeldWriters,
    session: &SessionId,
    event: &EventId,
    state: &NativeMailState,
    created_ms: i64,
) -> Result<(), InboxError> {
    if session.as_str().len() > 512
        || event.as_str().len() > 2048
        || state
            .target
            .as_ref()
            .is_some_and(|id| id.as_str().len() > 512)
    {
        return Err(InboxError::PendingBound("native mail state identities"));
    }
    let aggregate = AggregateId::Session(session.clone());
    let data = json!({"sessionID":state.session_id.as_str(),"held":state.held,"humanResume":state.human_resumed,"target":state.target.as_ref().map(EventId::as_str),"binding":state.binding});
    validate_state(&data).map_err(InboxError::PendingBound)?;
    if data.to_string().len() > 4096 {
        return Err(InboxError::PendingBound("native mail state bytes"));
    }
    EventStore::append_within(
        tx,
        AppendInput {
            id: event,
            aggregate: aggregate.clone(),
            writer: held.token(&aggregate)?,
            kind_name: "session.native-mail-state",
            version: EventVersion::new(1).ok_or(InboxError::Version)?,
            created_ms,
            data: &data,
        },
        &mut [&mut InboxProjector],
    )?;
    Ok(())
}

pub(super) fn pause(
    tx: &GovernedTransaction<'_>,
    held: &HeldWriters,
    session: &SessionId,
    created_ms: i64,
) -> Result<(), InboxError> {
    let (previous, mut state) = latest(tx, session)?;
    // Every explicit stop fences earlier resume requests, even when already held.
    // The committed sequence keeps consecutive stops bounded instead of nesting
    // the entire previous event identity into the next one.
    let pause_sequence = match previous {
        Some(previous) => tx
            .query_row(
                "SELECT seq FROM event WHERE id=?1",
                [previous.as_str()],
                |row| row.get::<_, i64>(0),
            )?
            .checked_add(1)
            .filter(|sequence| *sequence > 0)
            .ok_or(InboxError::PendingBound("native mail stop sequence"))?,
        None => 0,
    };
    state.held = true;
    state.human_resumed = false;
    let event: EventId = format!("evt_native_pause_{}_{}", session.as_str(), pause_sequence)
        .parse()
        .map_err(|_| InboxError::PendingBound("native mail pause identity"))?;
    append(tx, held, session, &event, &state, created_ms)
}

pub(super) fn admission_target(
    connection: &rusqlite::Connection,
    session: &SessionId,
    metadata: Option<&Value>,
) -> Result<Option<EventId>, InboxError> {
    let (_, state) = latest(connection, session)?;
    if !state.available()
        || state.binding.as_ref().is_some_and(|binding| {
            metadata
                .and_then(|m| m.get("binding"))
                .and_then(Value::as_str)
                != Some(binding.as_str())
        })
    {
        return Err(InboxError::PendingBound(
            "native mail target is held or superseded",
        ));
    }
    Ok(state.target)
}

impl Inbox {
    /// The current stop generation, not the selected target generation.
    pub fn native_mail_hold(&self, session: &SessionId) -> Result<Option<EventId>, InboxError> {
        let (event, state) = latest(self.store.connection(), session)?;
        Ok(if state.held { event } else { None })
    }

    /// Release a current selected target without admitting or promoting input.
    /// The stop check and append serialize with interruption and revocation.
    pub fn resume_native_mail_target(
        &mut self,
        request: NativeMailResume<'_>,
    ) -> Result<(), InboxError> {
        let held = self.store.held_writers();
        let tx = self.store.governed_transaction()?;
        let (previous, mut state) = latest(&tx, request.session)?;
        if !state.held
            || previous.as_ref() != Some(request.hold)
            || state.target.as_ref() != Some(request.target)
            || state.binding.as_deref() != Some(request.binding)
            || !binding_matches(&tx, request.session, request.binding)?
        {
            return Err(InboxError::PendingBound(
                "native mail stop or target is superseded",
            ));
        }
        state.held = false;
        state.human_resumed = true;
        append(
            &tx,
            &held,
            request.session,
            request.event,
            &state,
            request.created_ms,
        )?;
        tx.commit()?.discard();
        Ok(())
    }

    /// Current canonical external notice target; a pause is not a revocation.
    pub fn native_notice_target(
        &self,
        session: &SessionId,
    ) -> Result<Option<(EventId, String, bool)>, InboxError> {
        notice_target(self.store.connection(), session)
    }

    /// A genuine human promotion resumes already-held native work at the next
    /// boundary. Other queued inputs retain their ordinary idle-only delivery.
    pub(super) fn resumed_native_queue_within(
        connection: &rusqlite::Connection,
        session: &SessionId,
        input: &PendingInput,
    ) -> Result<bool, InboxError> {
        Ok(input.delivery == DeliveryMode::Queue
            && native_wake_admission(connection, session, input)?.is_some()
            && latest(connection, session)?.1.human_resumed
            && super::native_mail::native_wake_held(connection, session, &input.input)?)
    }

    /// One registry transition revokes old targets and establishes the new
    /// binding atomically. Human input can clear a pause, never a revocation.
    pub fn select_native_mail_target(
        &mut self,
        session: &SessionId,
        superseded: &[SessionId],
        binding: &str,
        event: &EventId,
        created_ms: i64,
    ) -> Result<(), InboxError> {
        if binding.len() != 64
            || !binding.bytes().all(|c| c.is_ascii_hexdigit())
            || session.as_str().len() > 512
            || event.as_str().len() > 512
        {
            return Err(InboxError::PendingBound("native mail target coordinates"));
        }
        if superseded.len() > 64 || superseded.iter().any(|id| id.as_str().len() > 512) {
            return Err(InboxError::PendingBound("native mail target count"));
        }
        let held = self.store.held_writers();
        let tx = self.store.governed_transaction()?;
        if !binding_matches(&tx, session, binding)? {
            return Err(InboxError::PendingBound(
                "native mail target binding changed",
            ));
        }
        for old in superseded {
            if old == session {
                continue;
            }
            let (_, mut state) = latest(&tx, old)?;
            state.held = true;
            state.human_resumed = false;
            // Keep a distinct revoked generation even for a legacy owner.
            state.target = Some(event.clone());
            state.binding = None;
            let revoked: EventId = format!("evt_native_revoke_{}_{}", old.as_str(), event.as_str())
                .parse()
                .map_err(|_| InboxError::PendingBound("native mail revocation identity"))?;
            append(&tx, &held, old, &revoked, &state, created_ms)?;
        }
        let (_, mut state) = latest(&tx, session)?;
        state.target = Some(event.clone());
        state.binding = Some(binding.to_owned());
        append(&tx, &held, session, event, &state, created_ms)?;
        tx.commit()?.discard();
        Ok(())
    }

    /// Revoke the old generation before changing capability or binding custody.
    pub fn revoke_native_mail_target(
        &mut self,
        session: &SessionId,
        event: &EventId,
        created_ms: i64,
    ) -> Result<(), InboxError> {
        let held = self.store.held_writers();
        let tx = self.store.governed_transaction()?;
        let (_, mut state) = latest(&tx, session)?;
        state.target = Some(event.clone());
        state.human_resumed = false;
        state.binding = None;
        append(&tx, &held, session, event, &state, created_ms)?;
        tx.commit()?.discard();
        Ok(())
    }

    pub fn native_mail_wakes_available(&self, session: &SessionId) -> Result<bool, InboxError> {
        Ok(latest(self.store.connection(), session)?.1.available())
    }

    /// Current selected targets for a restarted service to reopen, bounded in
    /// SQL. Revocation reaches only in-memory owners: newest per binding wins.
    pub fn selected_native_mail_sessions(
        &self,
        limit: usize,
    ) -> Result<Vec<SessionId>, InboxError> {
        let connection = self.store.connection();
        let rows: Vec<(String, String)> = connection
            .prepare(
                // Rank the latest row first, then read it: an unreadable latest
                // row drops only its own session and never revives an older one.
                "SELECT s, b FROM (SELECT s, b, ROW_NUMBER() OVER (PARTITION BY b ORDER BY c DESC, r DESC) m, c, r
                   FROM (SELECT s, c, r, CASE WHEN length(CAST(d AS BLOB))<=4096 AND json_valid(d)
                       THEN json_extract(d,'$.binding') END b FROM (SELECT aggregate_id s, data d, created c,
                       rowid r, ROW_NUMBER() OVER (PARTITION BY aggregate_id ORDER BY seq DESC) n FROM event
                       WHERE aggregate_family='session' AND type='session.native-mail-state.1')
                     WHERE n=1 AND length(CAST(s AS BLOB))<=512) WHERE typeof(b)='text')
                 WHERE m=1 ORDER BY c DESC, r DESC LIMIT ?1",
            )?
            .query_map([i64::try_from(limit).unwrap_or(i64::MAX)], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<Result<_, _>>()?;
        let mut selected = Vec::new();
        for (id, binding) in rows {
            // One undecodable row skips its own session, never every seat's.
            let Ok(session) = id.parse::<SessionId>() else {
                continue;
            };
            if binding_matches(connection, &session, &binding).unwrap_or(false) {
                selected.push(session);
            }
        }
        Ok(selected)
    }

    /// Called in the same transaction that promotes a selected human input.
    pub fn resume_native_mail_after_human_within(
        tx: &GovernedTransaction<'_>,
        held: &HeldWriters,
        session: &SessionId,
        input: &PendingInput,
        created_ms: i64,
    ) -> Result<(), InboxError> {
        if input.kind != PromotedInputKind::User
            || input
                .metadata
                .as_ref()
                .and_then(|m| m.get("origin"))
                .and_then(Value::as_str)
                == Some("seat-startup")
        {
            return Ok(());
        }
        let (_, mut state) = latest(tx, session)?;
        if !state.held && (state.human_resumed || !has_held_native_input(tx, session)?) {
            return Ok(());
        }
        state.held = false;
        state.human_resumed = true;
        let event: EventId = format!("evt_native_resume_{}", input.input.as_str())
            .parse()
            .map_err(|_| InboxError::PendingBound("native mail resume identity"))?;
        append(tx, held, session, &event, &state, created_ms)
    }

    /// Canonical provenance is outside caller-controlled metadata. A held or
    /// superseded notice stays pending while unrelated human input can advance.
    pub fn native_mail_input_eligible_within(
        connection: &rusqlite::Connection,
        session: &SessionId,
        input: &PendingInput,
    ) -> Result<bool, InboxError> {
        let Some(data) = native_wake_admission(connection, session, input)? else {
            return Ok(true);
        };
        let (_, state) = latest(connection, session)?;
        if !state.available()
            || (!state.human_resumed
                && super::native_mail::native_wake_held(connection, session, &input.input)?)
        {
            return Ok(false);
        }
        let stamp = data.pointer("/input/nativeMailTarget");
        let expected = state
            .target
            .as_ref()
            .map(|id| Value::String(id.to_string()))
            .unwrap_or(Value::Null);
        if stamp.unwrap_or(&Value::Null) != &expected {
            return Ok(false);
        }
        if let Some(binding) = state.binding {
            if !binding_matches(connection, session, &binding)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn has_held_native_input(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<bool, InboxError> {
    for pending in Inbox::pending_inputs_for_promotion_within(connection, session)? {
        if pending.kind == PromotedInputKind::Synthetic
            && super::native_mail::native_wake_held(connection, session, &pending.input)?
            && native_wake_admission(connection, session, &pending)?.is_some()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn native_wake_admission(
    connection: &rusqlite::Connection,
    session: &SessionId,
    input: &PendingInput,
) -> Result<Option<Value>, InboxError> {
    if input.kind != PromotedInputKind::Synthetic {
        return Ok(None);
    }
    let sequence = i64::try_from(input.seq)
        .map_err(|_| InboxError::PendingBound("native mail input sequence"))?;
    let raw: String = connection.query_row(
        "SELECT CASE WHEN length(CAST(data AS BLOB))<=4194304 THEN data END
         FROM event WHERE aggregate_family='session' AND aggregate_id=?1
           AND seq=?2 AND type='session.input.admitted.1'",
        rusqlite::params![session.as_str(), sequence],
        |r| r.get(0),
    )?;
    let data: Value = serde_json::from_str(&raw)
        .map_err(|_| InboxError::PendingBound("native mail admission encoding"))?;
    Ok((data.pointer("/input/nativeMailWake") == Some(&Value::Bool(true))).then_some(data))
}
