//! Retained tool rows derive from committed parent input and terminal facts.
use super::*;
use grapt_api::u3a::{ToolCalledData, ToolFailedData, ToolInputStartedData, ToolSuccessData};
use grapt_session::CommittedPosition;
use grapt_types::ToolCallId;

impl LiveSink {
    pub(super) fn reconcile_tool_publication(&self, reason: String) {
        // A legal committed result can exceed a volatile frame. Drain prior
        // valid frames and let the retained client's existing EOF/reconnect
        // hydration recover the complete result through bounded history.
        self.hub.reconcile_because(
            grapt_api::ReconcileReason::UnrepresentableFrame,
            &format!(
                "session {}: tool publication refused: {reason}",
                self.session
            ),
        );
        self.publication.refuse(self.session.clone(), reason);
    }

    pub(super) fn publish_tool_open(
        &mut self,
        captured: &CommittedPosition,
        message: &MessageId,
        call: &ToolCallId,
    ) -> Result<(), String> {
        if self.message.as_ref() != Some(message) {
            return Err("tool capture belongs to another assistant".into());
        }
        if self.tools.contains_key(call) {
            return Err("tool input was already published".into());
        }
        if self.tools.len() >= 128 {
            return Err("live parent tool bound exceeded".into());
        }
        let projection = super::super::continuation::tool_projection(
            &self.inbox,
            &self.artifacts,
            &self.session,
            call,
        )?;
        if projection.settled {
            return Err("tool opening arrived after its terminal".into());
        }
        let position = self.tool_position(
            call,
            "session.tool.input.started.1",
            Some("session.tool.input.started.2"),
            "$.callID",
        )?;
        if position.seq >= captured.seq {
            return Err("tool input identity does not precede its capture".into());
        }
        // Track the committed parent even if advisory input publication fails.
        // Reconnect can hydrate it running; its later terminal must still settle.
        self.tools.insert(call.clone(), false);
        let data = serde_json::json!({"sessionID":self.session.as_str(),
            "assistantMessageID":message.as_str(),"callID":call.as_str(),"name":projection.name});
        let started: ToolInputStartedData =
            serde_json::from_value(data.clone()).map_err(|e| e.to_string())?;
        let called = ToolCalledData::new(
            wire_session(&self.session).map_err(|e| e.to_string())?,
            wire_message(message).map_err(|e| e.to_string())?,
            WireId::parse(call.as_str().to_owned()).ok_or("tool identity")?,
            &serde_json::to_string(&projection.input).map_err(|e| e.to_string())?,
        )?;
        self.hub
            .publish(&DogfoodEvent::ToolInputStarted(durable(
                &self.session,
                &position,
                started,
                false,
            )?))
            .map_err(|e| e.to_string())?;
        self.hub
            .publish(&DogfoodEvent::ToolCalled(durable(
                &self.session,
                captured,
                called,
                false,
            )?))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub(super) fn publish_tool_terminal(&mut self, call: &ToolCallId) -> Result<(), String> {
        // Private nested calls never created model/UI tool content. Duplicate
        // callbacks cannot overwrite a settled row or resurrect progress.
        if self.tools.get(call) != Some(&false) {
            return Ok(());
        }
        let message = self
            .message
            .as_ref()
            .ok_or("tool terminal has no assistant")?;
        let projection = super::super::continuation::tool_projection(
            &self.inbox,
            &self.artifacts,
            &self.session,
            call,
        )?;
        if !projection.settled {
            return Err("tool terminal is not committed".into());
        }
        let raw = super::super::history_json::tool_state_json(&projection, &self.artifacts, true)?;
        let session = wire_session(&self.session).map_err(|e| e.to_string())?;
        let message = wire_message(message).map_err(|e| e.to_string())?;
        let identity = WireId::parse(call.as_str().to_owned()).ok_or("tool identity")?;
        let position = self.tool_position(call, "session.tool.terminal.1", None, "$.call")?;
        let event = if projection.error.is_some() {
            let state = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            let failed = ToolFailedData::from_history(session, message, identity, state)?;
            DogfoodEvent::ToolFailed(durable(&self.session, &position, failed, true)?)
        } else {
            let state = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            let success = ToolSuccessData::from_history(session, message, identity, state)?;
            DogfoodEvent::ToolSuccess(durable(&self.session, &position, success, true)?)
        };
        self.hub.publish(&event).map_err(|e| e.to_string())?;
        self.tools.insert(call.clone(), true);
        Ok(())
    }

    fn tool_position(
        &self,
        call: &ToolCallId,
        kind: &str,
        second_kind: Option<&str>,
        path: &str,
    ) -> Result<CommittedPosition, String> {
        let inbox = self
            .inbox
            .lock()
            .map_err(|_| "tool history lock poisoned")?;
        let position = grapt_store::event_publication::tool_position(
            inbox.store(),
            &self.session,
            call,
            kind,
            second_kind,
            path,
        )?;
        Ok(CommittedPosition {
            event: position.event,
            seq: position.seq,
            created_ms: position.created_ms,
        })
    }
}

fn durable<D>(
    session: &SessionId,
    position: &CommittedPosition,
    data: D,
    terminal: bool,
) -> Result<DurableEvent<D>, String> {
    let aggregate = AggregateIdDto::try_new(session.as_str())?;
    let reference = if terminal {
        DurableEventRef::tool_terminal(aggregate, position.seq.into())
    } else {
        DurableEventRef::new(aggregate, position.seq.into())
    };
    Ok(DurableEvent::new(
        WireId::parse(position.event.as_str().to_owned()).ok_or("tool event identity")?,
        FiniteF64::try_new(position.created_ms as f64)?,
        reference,
        data,
    ))
}
