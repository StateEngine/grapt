//! Read a settled conversation through its ordinary context reconstruction owner.
use super::*;

impl LiveExecutor {
    pub(super) fn side_context(
        &self,
        session: &SessionId,
        target: &ResolvedModelTarget,
        targets: &Arc<dyn super::super::production::ProductTargetAuthority>,
    ) -> Result<Vec<grapt_provider::CodexInputItem>, String> {
        if self.active_executions().iter().any(|(id, _)| id == session) {
            return Err("Wait for this response before asking a side question.".into());
        }
        let (head, input, mut instructions) = {
            let inbox = self
                .inbox
                .lock()
                .map_err(|_| "side context lock poisoned")?;
            let input = inbox
                .promoted_messages(session, grapt_session::MAX_TOOL_CALL_PAGE_ROWS)
                .map_err(|error| error.to_string())?
                .into_iter()
                .last()
                .ok_or("Complete a response in this Session before using /side.")?;
            let instructions = super::super::instructions::snapshot(
                &inbox,
                session,
                targets.protocol_family() != grapt_provider::ProtocolFamily::AnthropicMessages,
            )?;
            if instructions.is_empty() {
                return Err("Complete a response in this Session before using /side.".into());
            }
            (head(&inbox, session)?, input, instructions)
        };
        let continuation = DurableContinuation::new(
            Arc::clone(&self.inbox),
            Arc::clone(&self.location.tool_artifacts),
            session.clone(),
            input.input,
        );
        continuation.select_target_with_effort(
            target,
            targets.protocol_family(),
            targets.effort_policy(),
        );
        let (history, _, _) = continuation
            .request_with_instructions(&input.text, false, &[])
            .map_err(|error| error.to_string())?;
        let inbox = self
            .inbox
            .lock()
            .map_err(|_| "side context lock poisoned")?;
        if head != self::head(&inbox, session)? {
            return Err("The Session changed while preparing the side question; try again.".into());
        }
        instructions.push(grapt_provider::CodexInputItem::Message(grapt_provider::InputMessage {
            role: grapt_provider::MessageRole::System,
            content: "Answer the following side question from this conversation snapshot. This question and answer are not saved in the conversation. Answer in text; no tools are available.".into(),
        }));
        instructions.extend(history);
        Ok(instructions)
    }
}

fn head(inbox: &Inbox, session: &SessionId) -> Result<Option<u64>, String> {
    grapt_store::session_event_queries::bounds::head(inbox.store(), session)
        .map(|head| head.map(grapt_types::EventSeq::value))
}
