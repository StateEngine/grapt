//! Conversation branching uses the normal selection and history owners.
// Copyright 2025 OpenAI. Licensed under the Apache License, Version 2.0.
// Modified by Grapt: selected-prompt forks through the typed native API.
use super::*;

#[derive(Debug)]
pub struct ForkedSession {
    pub session: SessionWireId,
    pub prompt: Option<String>,
    pub attachment_error: Option<String>,
}

impl<C: Connection> Client<C> {
    /// None forks the latest history; Some(n) forks before the nth most recent
    /// initial prompt and stages that prompt for editing, without submitting it.
    pub fn fork_session(&mut self, prompts: Option<u64>) -> Result<ForkedSession, ClientError> {
        let parent = self.selected.clone().ok_or(ClientError::NoSelection)?;
        if self.pending.contains_key(&parent) {
            return Err(ClientError::Unconfirmed);
        }
        if prompts.is_none()
            && self.view().is_some_and(|view| {
                matches!(
                    view.execution,
                    crate::projection::ExecutionState::Running { .. }
                )
            })
        {
            return Err(ClientError::Body {
                operation: Operation::SessionFork.operation_id(),
                reason: "Wait for the response, or use /fork n to select an earlier prompt.".into(),
            });
        }
        let (body, prompt) = if let Some(prompts) = prompts {
            let body = crate::wire::encode_rewind_preview(prompts)?;
            let bytes = self.ingest(
                Operation::SessionRewindPreview,
                Some(&parent),
                None,
                Some(body),
                None,
                None,
            )?;
            let selected = crate::wire::decode_rewind_preview(&bytes)?;
            if selected.session != parent || selected.prompt.len() > self.bounds.max_prompt_bytes {
                return Err(ClientError::Body {
                    operation: Operation::SessionFork.operation_id(),
                    reason: "fork selection is foreign or exceeds the composer bound".into(),
                });
            }
            (
                crate::wire::encode_fork(Some(&selected.before))?,
                Some(selected.prompt),
            )
        } else {
            (crate::wire::encode_fork(None)?, None)
        };
        let bytes = self.ingest(
            Operation::SessionFork,
            Some(&parent),
            None,
            Some(body),
            None,
            None,
        )?;
        // Once a child is known, a failed refresh cannot cause a second fork.
        let row = crate::wire::decode_session(&bytes)?;
        if row.id == parent || row.parent.as_ref() != Some(&parent) {
            return Err(ClientError::Body {
                operation: Operation::SessionFork.operation_id(),
                reason: "fork receipt names a different parent or reuses its Session".into(),
            });
        }
        let session = row.id;
        let attachment_error = self.select(&session).err().map(|error| error.to_string());
        // The existing picker can recover an attachment failure independently.
        let _ = self.refresh_sessions();
        Ok(ForkedSession {
            session,
            prompt,
            attachment_error,
        })
    }
}
