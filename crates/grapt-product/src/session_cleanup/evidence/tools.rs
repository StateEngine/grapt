//! Tool settlement is independent of Step/execution settlement. Missing or
//! ambiguous results must not disappear behind a successfully closed execution.
use std::collections::{BTreeMap, BTreeSet};

use grapt_event::{
    session_message, session_tool_identity, session_tool_nested, session_tool_terminal, EventKind,
};
use grapt_types::{
    ContentKind, MessageId, PhysicalAttemptId, SessionId, ToolCallId, ToolFailureReason,
    ToolTerminalOutcome,
};
use serde_json::Value;

#[derive(Debug, Default)]
pub(super) struct Tools {
    local: BTreeMap<ToolCallId, MessageId>,
    pending: BTreeSet<ToolCallId>,
    nested: BTreeSet<(PhysicalAttemptId, ToolCallId)>,
    seen_nested: BTreeSet<(PhysicalAttemptId, ToolCallId)>,
    messages: BTreeSet<MessageId>,
    attempts: BTreeMap<PhysicalAttemptId, MessageId>,
    content_calls: BTreeSet<ToolCallId>,
    ambiguous: bool,
}

impl Tools {
    pub(super) fn message(&mut self, kind: EventKind, data: &Value) -> Result<(), String> {
        let (attempt, message) = match kind {
            EventKind::SessionAssistantMessageOpenedV1
            | EventKind::SessionAssistantMessageOpenedV2 => {
                let fact = if kind == EventKind::SessionAssistantMessageOpenedV2 {
                    session_message::decode_opened_v2(data)
                } else {
                    session_message::decode_opened(data)
                }
                .map_err(|error| error.to_string())?;
                self.messages.insert(fact.message().clone());
                (fact.attempt().clone(), fact.message().clone())
            }
            EventKind::SessionContentBlockOpenedV1 => {
                let fact = session_message::decode_block_opened(data)
                    .map_err(|error| error.to_string())?;
                (fact.attempt().clone(), fact.message().clone())
            }
            _ => return Ok(()),
        };
        self.bind(attempt, message)
    }

    fn bind(&mut self, attempt: PhysicalAttemptId, message: MessageId) -> Result<(), String> {
        if self
            .attempts
            .get(&attempt)
            .is_some_and(|prior| prior != &message)
        {
            return Err("tool attempt belongs to a different assistant message".into());
        }
        self.attempts.insert(attempt, message);
        Ok(())
    }

    pub(super) fn observe(
        &mut self,
        session: &SessionId,
        kind: EventKind,
        data: &Value,
    ) -> Result<(), String> {
        match kind {
            EventKind::SessionToolInputStartedV1 | EventKind::SessionToolInputStartedV2 => {
                let identity = session_tool_identity::decode(kind.version().value(), data)
                    .map_err(|error| error.to_string())?;
                if identity.session != *session
                    || !self.messages.contains(&identity.message)
                    || self
                        .local
                        .insert(identity.call.clone(), identity.message)
                        .is_some()
                    || self
                        .seen_nested
                        .iter()
                        .any(|(_, call)| call == &identity.call)
                {
                    return Err("tool identity is duplicated or differs from its aggregate".into());
                }
                self.pending.insert(identity.call);
            }
            EventKind::SessionToolNestedStartedV1 => {
                let fact = session_tool_nested::decode(data).map_err(|error| error.to_string())?;
                let key = (fact.attempt().clone(), fact.call().clone());
                if self.local.contains_key(fact.call())
                    || !self.seen_nested.insert(key.clone())
                    || !(self.pending.contains(fact.parent())
                        || self
                            .nested
                            .contains(&(fact.attempt().clone(), fact.parent().clone())))
                {
                    return Err(
                        "nested tool admission has no pending parent or repeats an identity".into(),
                    );
                }
                self.nested.insert(key);
            }
            EventKind::SessionToolTerminalV1 => {
                let fact =
                    session_tool_terminal::decode(data).map_err(|error| error.to_string())?;
                let nested = self
                    .nested
                    .remove(&(fact.attempt().clone(), fact.call().clone()));
                if !nested && !self.pending.remove(fact.call()) {
                    return Err("tool terminal has no matching pending admission".into());
                }
                if !nested {
                    let message = self
                        .local
                        .get(fact.call())
                        .ok_or("tool terminal lacks input identity")?
                        .clone();
                    // Like the history owner, an otherwise unbound retry may
                    // bind through its tool terminal; an existing binding cannot
                    // be reassigned to make an unrelated terminal settle work.
                    self.bind(fact.attempt().clone(), message)?;
                }
                self.ambiguous |= matches!(
                    fact.outcome(),
                    ToolTerminalOutcome::Failed {
                        reason: ToolFailureReason::EffectUnknown | ToolFailureReason::CaptureFailed,
                        ..
                    } | ToolTerminalOutcome::ResultMissing
                );
            }
            _ => return Err("unsupported tool lifecycle".into()),
        }
        Ok(())
    }

    pub(super) fn content(&mut self, data: &Value) -> Result<(), String> {
        let fact = session_message::decode_appended(data).map_err(|error| error.to_string())?;
        if let ContentKind::Tool { call } = fact.item().kind() {
            if self.local.get(call) != Some(fact.message())
                || !self.pending.contains(call)
                || !self.content_calls.insert(call.clone())
            {
                return Err("tool content has no matching pending input identity".into());
            }
        }
        Ok(())
    }

    pub(super) fn unresolved(&self) -> bool {
        self.ambiguous || !self.pending.is_empty() || !self.nested.is_empty()
    }
}
