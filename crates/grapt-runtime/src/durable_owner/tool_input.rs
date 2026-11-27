//! Tool identity and input are one synchronous intent segment.
use super::*;

pub(super) struct Identity {
    id: EventId,
    version: grapt_types::EventVersion,
    created_ms: i64,
    fact: grapt_event::session_tool_identity::ToolInputIdentity,
}
impl Identity {
    pub(super) fn record(
        &self,
        tx: &grapt_store::GovernedTransaction<'_>,
        session: &grapt_types::SessionId,
        writer: FencedWriter,
    ) -> Result<(), OwnerError> {
        let data = grapt_event::session_tool_identity::encode(&self.fact);
        EventStore::append_within(
            tx,
            grapt_store::AppendInput {
                id: &self.id,
                aggregate: AggregateId::Session(session.clone()),
                writer,
                kind_name: "session.tool.input.started",
                version: self.version,
                created_ms: self.created_ms,
                data: &data,
            },
            &mut [],
        )
        .map_err(write)?;
        Ok(())
    }
}
impl<A: ArtifactStore, S: CommittedFactSink> DurableAttemptOwner<'_, A, S> {
    // Validate and stamp before capture; the identity is published with its
    // input in the caller's transaction, before any local tool can start.
    pub(super) fn tool_identity(
        &self,
        call: &ToolCallId,
        tool: &str,
        execution: grapt_types::ToolExecution,
    ) -> Result<Identity, OwnerError> {
        if grapt_provider::ToolName::new(tool).is_none() {
            return Err(OwnerError {
                reason: "tool name exceeds its canonical input bound".into(),
            });
        }
        let message = self.message.as_ref().ok_or_else(|| OwnerError {
            reason: "tool input arrived before its assistant opening".into(),
        })?;
        let tool = grapt_types::ToolDefinitionId::new(tool).ok_or_else(|| OwnerError {
            reason: "tool definition identity is invalid".into(),
        })?;
        let fact = grapt_event::session_tool_identity::ToolInputIdentity {
            session: self.session.clone(),
            message: message.clone(),
            call: call.clone(),
            tool,
            execution,
        };
        let id = self.event_id()?;
        let now = self.now_ms();
        let version = grapt_types::EventVersion::new(2).ok_or_else(|| OwnerError {
            reason: "tool identity version is invalid".into(),
        })?;
        Ok(Identity {
            id,
            version,
            created_ms: now,
            fact,
        })
    }
}
