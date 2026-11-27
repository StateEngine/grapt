// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: durable inbox admission and bounded family validation.

use super::*;
use grapt_store::{agent_message::AgentMessage, session_tool_identity};
use grapt_store::{AggregateId, AppendInput, Appended, GovernedTransaction, HeldWriters};
use grapt_types::{EventVersion, ToolExecution};
use rusqlite::OptionalExtension;

#[derive(Debug)]
pub struct AgentMessageRequest<'a> {
    pub sender: &'a SessionId,
    pub target: &'a SessionId,
    pub call: &'a ToolCallId,
    pub text: &'a str,
    pub created_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentMessageReceipt {
    pub input: MessageId,
    pub seq: u64,
}

fn refused(reason: impl Into<String>) -> SubagentError {
    SubagentError::AgentMessage {
        reason: reason.into(),
    }
}

fn root(conn: &rusqlite::Connection, session: &SessionId) -> Result<SessionId, SubagentError> {
    let mut current = session.clone();
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..32 {
        if current.as_str().len() > 512 || !seen.insert(current.clone()) {
            return Err(refused("agent family has an oversized address or a cycle"));
        }
        match SubagentService::child_context_within(conn, &current)? {
            Some(context) => current = context.parent,
            None => return Ok(current),
        }
    }
    Err(refused("agent family exceeds 32 levels"))
}

pub(super) fn authorize(
    conn: &rusqlite::Connection,
    sender: &SessionId,
    target: &SessionId,
) -> Result<(), SubagentError> {
    if root(conn, sender)? != root(conn, target)? {
        return Err(refused("the target is outside this agent family"));
    }
    Ok(())
}

pub(super) fn source(
    conn: &rusqlite::Connection,
    sender: &SessionId,
    call: &ToolCallId,
    tool: &str,
) -> Result<EventId, SubagentError> {
    if call.as_str().len() > 512 {
        return Err(refused("sender call exceeds 512 bytes"));
    }
    let mut query = conn.prepare(
        "SELECT CASE WHEN length(CAST(id AS BLOB))<=480 THEN id END,
            CASE WHEN length(CAST(type AS BLOB))<=256 THEN type END,
            CASE WHEN length(CAST(data AS BLOB))<=16384 THEN data END
         FROM event WHERE aggregate_family='session' AND aggregate_id=?1
            AND type LIKE 'session.tool.input.started.%'
            AND CASE WHEN json_valid(data) THEN json_extract(data,'$.callID')=?2 ELSE 1 END
         ORDER BY seq LIMIT 2",
    )?;
    let mut rows = query.query((sender.as_str(), call.as_str()))?;
    let row = rows
        .next()?
        .ok_or_else(|| refused("the sender call has no canonical manifest"))?;
    let id: Option<String> = row.get(0)?;
    let kind: Option<String> = row.get(1)?;
    let raw: Option<String> = row.get(2)?;
    let version = match kind.as_deref() {
        Some("session.tool.input.started.1") => 1,
        Some("session.tool.input.started.2") => 2,
        _ => return Err(refused("the sender manifest has an unknown version")),
    };
    let data =
        serde_json::from_str(&raw.ok_or_else(|| refused("sender manifest exceeds its bound"))?)
            .map_err(|error| refused(format!("sender manifest JSON: {error}")))?;
    let identity = session_tool_identity::decode(version, &data)
        .map_err(|error| refused(error.to_string()))?;
    if identity.session != *sender
        || identity.call != *call
        || identity.tool.as_str() != tool
        || identity.execution != ToolExecution::Local
    {
        return Err(refused(
            "sender manifest does not name this local message call",
        ));
    }
    if rows.next()?.is_some() {
        return Err(refused("the sender call has repeated manifests"));
    }
    id.ok_or_else(|| refused("sender manifest identity exceeds its bound"))?
        .parse()
        .map_err(|error| refused(format!("sender manifest identity: {error}")))
}

pub(super) fn validate_request(request: &AgentMessageRequest<'_>) -> Result<(), SubagentError> {
    if request.text.len() > grapt_store::agent_message::MAX_TEXT_BYTES
        || request.text.trim().is_empty()
        || request.sender.as_str().len() > 512
        || request.target.as_str().len() > 512
        || request.call.as_str().len() > 512
        || request.created_ms < 0
    {
        return Err(refused(
            "agent message exceeds its text, identity or time bounds",
        ));
    }
    Ok(())
}

impl SubagentService {
    /// A read-only check before the product elects the recipient's writer.
    /// Admission repeats it under its immediate transaction.
    pub fn authorize_message_on(
        store: &EventStore,
        sender: &SessionId,
        target: &SessionId,
    ) -> Result<(), SubagentError> {
        authorize(store.connection(), sender, target)
    }

    /// Queue one non-user message without starting or re-prompting its recipient.
    /// The tool manifest supplies a stable retry identity; changed retries refuse.
    pub fn send_message_on(
        store: &mut EventStore,
        request: &AgentMessageRequest<'_>,
    ) -> Result<AgentMessageReceipt, SubagentError> {
        validate_request(request)?;
        let held = store.held_writers();
        let tx = store.governed_transaction()?;
        let receipt = Self::admit_message_within(&tx, &held, request, "send_message")?;
        tx.commit()?.discard();
        Ok(receipt)
    }

    pub(super) fn admit_message_within(
        tx: &GovernedTransaction<'_>,
        held: &HeldWriters,
        request: &AgentMessageRequest<'_>,
        tool: &str,
    ) -> Result<AgentMessageReceipt, SubagentError> {
        validate_request(request)?;
        let aggregate = AggregateId::Session(request.target.clone());
        let writer = held
            .token(&aggregate)
            .map_err(|error| refused(error.to_string()))?;
        authorize(tx, request.sender, request.target)?;
        let source = source(tx, request.sender, request.call, tool)?;
        let input: MessageId = format!("msg_agent_mail_{}", source.as_str())
            .parse()
            .map_err(|error| refused(format!("mail identity: {error}")))?;
        let event: EventId = format!("evt_agent_message_{}", source.as_str())
            .parse()
            .map_err(|error| refused(format!("mail event identity: {error}")))?;
        let message = AgentMessage {
            session: request.target.clone(),
            input: input.clone(),
            sender: request.sender.clone(),
            call: request.call.clone(),
            text: request.text.to_owned(),
        };
        let data = message.encode();
        AgentMessage::decode(&data).map_err(refused)?;
        let existing: Option<(Option<String>, Option<String>, i64)> = tx.query_row(
            "SELECT CASE WHEN length(CAST(aggregate_id AS BLOB))<=512 THEN aggregate_id END,
                CASE WHEN length(CAST(data AS BLOB))<=397312 THEN data END, seq
             FROM event WHERE id=?1 AND aggregate_family='session' AND type='session.input.admitted.1'",
            [event.as_str()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional()?;
        if let Some((target, raw, seq)) = existing {
            let prior: serde_json::Value = serde_json::from_str(
                &raw.ok_or_else(|| refused("prior agent message exceeds its bound"))?,
            )
            .map_err(|error| refused(error.to_string()))?;
            if target.as_deref() != Some(request.target.as_str()) || prior != data {
                return Err(refused(
                    "the sender call already queued a different message",
                ));
            }
            let seq = u64::try_from(seq).map_err(|_| refused("negative mail sequence"))?;
            return Ok(AgentMessageReceipt { input, seq });
        }
        let (count, bytes): (i64, i64) = tx.query_row(
            "SELECT COUNT(*), COALESCE(SUM(length(CAST(e.data AS BLOB))),0) FROM event e
             WHERE e.aggregate_family='session' AND e.aggregate_id=?1 AND e.type='session.input.admitted.1'
             AND json_extract(e.data,'$.input.type')='agent-message'
             AND NOT EXISTS(SELECT 1 FROM event c WHERE c.id='evt_agent_mail_' || json_extract(e.data,'$.inputID'))",
            [request.target.as_str()], |row| Ok((row.get(0)?,row.get(1)?)))?;
        let encoded = serde_json::to_vec(&data).map_err(|error| refused(error.to_string()))?;
        if !(0..64).contains(&count)
            || usize::try_from(bytes)
                .ok()
                .and_then(|bytes| bytes.checked_add(encoded.len()))
                .is_none_or(|bytes| bytes > 1024 * 1024)
        {
            return Err(refused("the recipient's agent mailbox is full"));
        }
        let appended = EventStore::append_within(
            tx,
            AppendInput {
                id: &event,
                aggregate,
                writer,
                kind_name: "session.input.admitted",
                version: EventVersion::new(1).ok_or_else(|| refused("mail version"))?,
                created_ms: request.created_ms,
                data: &data,
            },
            &mut [&mut grapt_inbox::InboxProjector],
        )
        .map_err(|error| refused(error.to_string()))?;
        let seq = match appended {
            Appended::Committed { seq } | Appended::Idempotent { seq } => seq,
        };
        Ok(AgentMessageReceipt { input, seq })
    }
}
