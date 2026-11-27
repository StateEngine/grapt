use super::*;
use grapt_event::session_tool_nested::{self, NestedToolStartedFact};
use grapt_types::{ToolDefinitionId, ToolSchemaVersion, ToolTerminalArtifact};

impl<A: ArtifactStore, S: CommittedFactSink> DurableAttemptOwner<'_, A, S> {
    pub(super) fn append_nested(
        &mut self,
        parent: &ToolCallId,
        call: &ToolCallId,
        tool: &ToolDefinitionId,
        catalog: &ToolSchemaVersion,
        input: &crate::BoundedToolInput,
    ) -> Result<(), OwnerError> {
        let attempt = self.attempt.clone().ok_or_else(|| OwnerError {
            reason: "nested call preceded attempt".into(),
        })?;
        let bound = CaptureBound::new(MAX_TOOL_INPUT_CAPTURE_BYTES, BLOCK_CAPTURE_BUFFER_BYTES)
            .ok_or_else(|| OwnerError {
                reason: "nested input bound invalid".into(),
            })?;
        let captured = self
            .artifacts
            .capture(
                input.as_bytes(),
                bound,
                ArtifactKind::CapturedOutput,
                Sensitivity::SecretBearing,
            )
            .map_err(|error| OwnerError {
                reason: format!("nested input capture failed: {error}"),
            })?;
        let input = ToolTerminalArtifact::new(captured.reference().clone(), captured.disposition())
            .ok_or_else(|| OwnerError {
                reason: "nested input capture disagrees".into(),
            })?;
        let fact = NestedToolStartedFact::new(
            attempt,
            parent.clone(),
            call.clone(),
            tool.clone(),
            catalog.clone(),
            input,
        )
        .map_err(|error| OwnerError {
            reason: error.to_string(),
        })?;
        let data = session_tool_nested::encode(&fact);
        let id = self.event_id()?;
        let now = self.now_ms();
        let mut guard = self.store.lock();
        let writer = self.writer(&guard)?;
        let tx = guard.governed_transaction().map_err(|error| OwnerError {
            reason: error.to_string(),
        })?;
        let version = grapt_types::EventVersion::new(1).ok_or_else(|| OwnerError {
            reason: "nested event version invalid".into(),
        })?;
        grapt_store::EventStore::append_within(
            &tx,
            grapt_store::AppendInput {
                id: &id,
                aggregate: AggregateId::Session(self.session.clone()),
                writer,
                kind_name: "session.tool.nested.started",
                version,
                created_ms: now,
                data: &data,
            },
            &mut [],
        )
        .map_err(write)?;
        tx.commit()
            .map_err(|error| OwnerError {
                reason: error.to_string(),
            })?
            .discard();
        Ok(())
    }
}
