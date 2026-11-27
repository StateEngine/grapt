//! Bounded wake intent admission and cancellation under the subagent owner.
use super::*;
use rusqlite::OptionalExtension;

pub(in crate::service) fn highest_generation(
    conn: &rusqlite::Connection,
) -> Result<u64, SubagentError> {
    if !installed(conn)? {
        return Ok(0);
    }
    let highest: Option<i64> = conn.query_row(
        "SELECT MAX(owner_generation) FROM subagent_followup",
        [],
        |row| row.get(0),
    )?;
    match highest {
        None => Ok(0),
        Some(value) => u64::try_from(value)
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| refused("invalid retained followup generation")),
    }
}

fn queued(
    conn: &rusqlite::Connection,
    child: &SessionId,
) -> Result<Vec<FollowupReceipt>, SubagentError> {
    if child.as_str().len() > 512 {
        return Err(refused("followup child exceeds its read bound"));
    }
    if !installed(conn)? {
        return Ok(Vec::new());
    }
    let mut query=conn.prepare("SELECT CASE WHEN length(CAST(message_id AS BLOB))<=512 THEN message_id END
        FROM subagent_followup WHERE child_session_id=?1 AND state='queued' ORDER BY admitted_seq LIMIT 5")?;
    let mut cursor = query.query([child.as_str()])?;
    let mut result = Vec::new();
    while let Some(row) = cursor.next()? {
        if result.len() == 4 {
            return Err(refused("child exceeds four retained followup wakes"));
        }
        let message: MessageId = row
            .get::<_, Option<String>>(0)?
            .ok_or_else(|| refused("followup identity exceeds its read bound"))?
            .parse()
            .map_err(|error| refused(format!("followup identity: {error}")))?;
        let stored =
            intent::read(conn, &message)?.ok_or_else(|| refused("queued followup disappeared"))?;
        if stored.child != *child || stored.receipt.state != FollowupState::Queued {
            return Err(refused("queued followup changed its child or state"));
        }
        result.push(stored.receipt);
    }
    Ok(result)
}

pub(super) fn next(
    conn: &rusqlite::Connection,
    child: &SessionId,
) -> Result<Option<FollowupReceipt>, SubagentError> {
    Ok(queued(conn, child)?.into_iter().next())
}

impl SubagentService {
    /// Reconcile an exact retry without reserving another process-local wake.
    /// Source and body remain checked even when admission capacity is full.
    pub fn existing_followup_on(
        store: &EventStore,
        message: &AgentMessageRequest<'_>,
    ) -> Result<Option<FollowupReceipt>, SubagentError> {
        super::super::message::validate_request(message)?;
        let conn = store.connection();
        super::super::message::authorize(conn, message.sender, message.target)?;
        let source =
            super::super::message::source(conn, message.sender, message.call, "followup_task")?;
        let input: MessageId = format!("msg_agent_mail_{}", source.as_str())
            .parse()
            .map_err(|error| refused(format!("followup message identity: {error}")))?;
        let Some(stored) = intent::read(conn, &input)? else {
            return Ok(None);
        };
        if stored.child != *message.target
            || stored.sender != *message.sender
            || stored.text != message.text
        {
            return Err(refused(
                "the sender call already queued a different followup",
            ));
        }
        Ok(Some(stored.receipt))
    }
    /// FIFO selection precedes acquisition of the child's execution mutex.
    pub fn next_followup_on(
        store: &EventStore,
        child: &SessionId,
    ) -> Result<Option<FollowupReceipt>, SubagentError> {
        next(store.connection(), child)
    }

    pub fn queued_followups_on(
        store: &EventStore,
        child: &SessionId,
    ) -> Result<usize, SubagentError> {
        Ok(queued(store.connection(), child)?.len())
    }

    /// Admit mail and its wake custody atomically. An exact retry returns the
    /// existing state and must not receive a second process-local wake owner.
    pub fn queue_followup_on(
        store: &mut EventStore,
        message: &AgentMessageRequest<'_>,
        generation: RuntimeGeneration,
    ) -> Result<FollowupReceipt, SubagentError> {
        super::super::message::validate_request(message)?;
        let generation = generation_to_i64(generation)?;
        if generation <= 0 {
            return Err(refused("followup requires a runtime generation"));
        }
        let held = store.held_writers();
        let tx = store.governed_transaction()?;
        super::super::message::authorize(&tx, message.sender, message.target)?;
        let source =
            super::super::message::source(&tx, message.sender, message.call, "followup_task")?;
        let input: MessageId = format!("msg_agent_mail_{}", source.as_str())
            .parse()
            .map_err(|error| refused(format!("followup message identity: {error}")))?;
        if let Some(existing) = intent::read(&tx, &input)? {
            Self::admit_message_within(&tx, &held, message, "followup_task")?;
            tx.commit()?.discard();
            return Ok(existing.receipt);
        }
        let chain = read_chain(&tx, message.target)?
            .ok_or_else(|| refused("followup cannot target a root Session"))?;
        if matches!(
            chain.current.status,
            OperationStatus::Admitted | OperationStatus::ExternallyAmbiguous(_)
        ) {
            return Err(refused("the child must finish admission or reconcile its ambiguous operation before followup"));
        }
        let (pending, total): (i64, i64) = tx.query_row(
            "SELECT COALESCE(SUM(child_session_id=?1),0),COUNT(*)
             FROM subagent_followup WHERE state='queued'",
            [message.target.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if !(0..MAX_PENDING).contains(&pending) || !(0..64).contains(&total) {
            return Err(refused("followup wake capacity is unavailable"));
        }
        let mail = Self::admit_message_within(&tx, &held, message, "followup_task")?;
        tx.execute(
            "INSERT INTO subagent_followup(message_id,child_session_id,sender_session_id,
              call_id,admitted_seq,owner_generation,state,time_created,time_updated)
             VALUES (?1,?2,?3,?4,?5,?6,'queued',?7,?7)",
            rusqlite::params![
                mail.input.as_str(),
                message.target.as_str(),
                message.sender.as_str(),
                message.call.as_str(),
                i64::try_from(mail.seq).map_err(|_| refused("followup position overflow"))?,
                generation,
                message.created_ms
            ],
        )?;
        tx.commit()?.discard();
        Ok(FollowupReceipt {
            message: mail.input,
            seq: mail.seq,
            state: FollowupState::Queued,
            admitted: true,
        })
    }

    pub fn followup_on(
        store: &EventStore,
        message: &MessageId,
    ) -> Result<Option<FollowupReceipt>, SubagentError> {
        Ok(intent::read(store.connection(), message)?.map(|intent| intent.receipt))
    }

    pub fn followup_run_on(
        store: &EventStore,
        operation: &SubagentOperationId,
    ) -> Result<Option<FollowupRun>, SubagentError> {
        Ok(intent::run(store.connection(), operation)?.map(|binding| binding.run))
    }

    pub fn followup_cause_on(
        store: &EventStore,
        operation: &SubagentOperationId,
    ) -> Result<Option<MessageId>, SubagentError> {
        Ok(intent::run(store.connection(), operation)?.map(|binding| binding.cause))
    }

    pub fn latest_followup_on(
        store: &EventStore,
        child: &SessionId,
    ) -> Result<Option<FollowupReceipt>, SubagentError> {
        let conn = store.connection();
        if !installed(conn)? {
            return Ok(None);
        }
        let raw: Option<Option<String>> = conn
            .query_row(
                "SELECT CASE WHEN length(CAST(message_id AS BLOB))<=512 THEN message_id END
             FROM subagent_followup WHERE child_session_id=?1 ORDER BY admitted_seq DESC LIMIT 1",
                [child.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(raw) = raw else { return Ok(None) };
        let message: MessageId = raw
            .ok_or_else(|| refused("followup address exceeds its read bound"))?
            .parse()
            .map_err(|error| refused(format!("followup address: {error}")))?;
        let stored = intent::read(conn, &message)?
            .ok_or_else(|| refused("followup disappeared during listing"))?;
        if stored.child != *child {
            return Err(refused("listed followup changed its child"));
        }
        Ok(Some(stored.receipt))
    }

    pub fn cancel_followup_on(
        store: &mut EventStore,
        message: &MessageId,
        generation: RuntimeGeneration,
        reason: FollowupCancelReason,
        created_ms: i64,
    ) -> Result<usize, SubagentError> {
        cancel(
            store,
            CancelScope::Owned(message, generation),
            reason,
            created_ms,
        )
    }

    pub fn cancel_child_followups_on(
        store: &mut EventStore,
        child: &SessionId,
        created_ms: i64,
    ) -> Result<usize, SubagentError> {
        cancel(
            store,
            CancelScope::Child(child),
            FollowupCancelReason::Interrupted,
            created_ms,
        )
    }

    pub fn cancel_stale_followups_on(
        store: &mut EventStore,
        generation: RuntimeGeneration,
        created_ms: i64,
    ) -> Result<usize, SubagentError> {
        cancel(
            store,
            CancelScope::Stale(generation),
            FollowupCancelReason::ProcessLoss,
            created_ms,
        )
    }
}

enum CancelScope<'a> {
    Owned(&'a MessageId, RuntimeGeneration),
    Child(&'a SessionId),
    Stale(RuntimeGeneration),
}

fn cancel(
    store: &mut EventStore,
    scope: CancelScope<'_>,
    reason: FollowupCancelReason,
    created_ms: i64,
) -> Result<usize, SubagentError> {
    if created_ms < 0 {
        return Err(refused("negative followup cancellation time"));
    }
    if !installed(store.connection())? {
        return Ok(0);
    }
    let (message, generation, child, stale) = match scope {
        CancelScope::Owned(message, generation) => (
            Some(message.as_str()),
            generation_to_i64(generation)?,
            None,
            false,
        ),
        CancelScope::Child(child) => (None, 0, Some(child.as_str()), false),
        CancelScope::Stale(generation) => (None, generation_to_i64(generation)?, None, true),
    };
    if generation < 0 || ((message.is_some() || stale) && generation == 0) {
        return Err(refused(
            "followup cancellation requires a runtime generation",
        ));
    }
    if message.is_some_and(|id| id.len() > 512) || child.is_some_and(|id| id.len() > 512) {
        return Err(refused("followup cancellation address exceeds its bound"));
    }
    let pending: bool = store.connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM subagent_followup WHERE state='queued' AND (
          (?1 IS NOT NULL AND message_id=?1 AND owner_generation=?2)
          OR (?3 IS NOT NULL AND child_session_id=?3)
          OR (?4 AND owner_generation<?2)))",
        rusqlite::params![message, generation, child, stale],
        |row| row.get(0),
    )?;
    if !pending {
        return Ok(0);
    }
    let tx = store.governed_transaction()?;
    let addresses = {
        let mut query = tx.prepare(
            "SELECT CASE WHEN length(CAST(message_id AS BLOB))<=512 THEN message_id END
             FROM subagent_followup WHERE state='queued' AND (
              (?1 IS NOT NULL AND message_id=?1 AND owner_generation=?2)
              OR (?3 IS NOT NULL AND child_session_id=?3)
              OR (?4 AND owner_generation<?2)) ORDER BY admitted_seq LIMIT 65",
        )?;
        let addresses = query
            .query_map(
                rusqlite::params![message, generation, child, stale],
                |row| row.get::<_, Option<String>>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        addresses
    };
    if addresses.len() > 64 {
        return Err(refused(
            "followup cancellation exceeds the retained wake bound",
        ));
    }
    let mut changed = 0;
    for raw in addresses {
        let message: MessageId = raw
            .ok_or_else(|| refused("followup address exceeds its read bound"))?
            .parse()
            .map_err(|error| refused(format!("followup cancellation address: {error}")))?;
        let stored = intent::read(&tx, &message)?
            .ok_or_else(|| refused("followup disappeared during cancellation"))?;
        if stored.receipt.state != FollowupState::Queued {
            return Err(refused("followup changed during cancellation"));
        }
        let (state, reason) = if intent::captured(&tx, &stored)? {
            ("consumed", None)
        } else {
            ("cancelled", Some(reason.as_str()))
        };
        changed += tx.execute(
            "UPDATE subagent_followup SET state=?2,reason=?3,time_updated=?4
             WHERE message_id=?1 AND state='queued'",
            rusqlite::params![message.as_str(), state, reason, created_ms],
        )?;
    }
    tx.commit()?.discard();
    Ok(changed)
}
