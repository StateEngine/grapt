//! Replay committed reasoning only for the selected endpoint's declared contract.
use super::*;
use grapt_provider::CompatibleReasoningReplay;
use grapt_session::history::{AssistantMessage, CapturedContentItem};

impl DurableContinuation {
    pub(in crate::dogfood) fn select_compatible_reasoning(
        &self,
        policy: CompatibleReasoningReplay,
    ) {
        *lock(&self.compatible_reasoning) = policy;
    }

    pub(super) fn compatible_reasoning_item(
        &self,
        message: &AssistantMessage,
        held: &CapturedContentItem,
        ceiling: u64,
        rows: &mut Vec<(u64, CodexInputItem)>,
        bytes: &mut usize,
    ) -> Result<bool, ContextError> {
        if !matches!(held.item.kind(), grapt_types::ContentKind::Reasoning) {
            return Ok(false);
        }
        if *lock(&self.compatible_reasoning) != CompatibleReasoningReplay::ReasoningContent {
            return Ok(true);
        }
        let selected = lock(&self.redeemer);
        let Some(target) = selected.as_ref() else {
            return Err("compatible reasoning has no selected target".into());
        };
        let selection = message
            .selection
            .as_ref()
            .ok_or("compatible reasoning has no durable selection")?;
        if selection.provider() != target.model().provider()
            || selection.model() != target.model().model()
        {
            return Ok(true);
        }
        let position = held
            .committed
            .as_ref()
            .ok_or("compatible reasoning has no durable position")?;
        if position.seq < ceiling {
            let text = read_text(
                &self.artifacts,
                held.item.payload(),
                held.disposition,
                "compatible reasoning",
            )?;
            push_context(
                rows,
                bytes,
                position.seq,
                CodexInputItem::CompatibleReasoning(text),
            )?;
        }
        Ok(true)
    }
}
