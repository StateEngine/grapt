//! Conversation editing is separate from prompt submission and file undo.
// Copyright 2025 OpenAI. Licensed under the Apache License, Version 2.0.
// Modified by Grapt: adapted app_backtrack.rs prompt reopening to the typed
// native API, exact-source reconciliation and bounded durable history reload.
use super::*;

#[derive(Debug)]
pub struct RewindEdit {
    pub prompt: String,
    pub history: RewindHistory,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RewindHistory {
    Refreshed,
    Recovering,
    Failed(String),
}

impl<C: Connection> Client<C> {
    pub fn rewind(&mut self, prompts: u64) -> Result<RewindEdit, ClientError> {
        let session = self.selected.clone().ok_or(ClientError::NoSelection)?;
        if self.undo_uncertain {
            return Err(ClientError::UndoUnconfirmed);
        }
        if self.rewind_uncertain.is_some() {
            return Err(ClientError::RewindUnconfirmed);
        }
        if self.pending.contains_key(&session) {
            return Err(ClientError::Unconfirmed);
        }
        let preview_body = crate::wire::encode_rewind_preview(prompts)?;
        let preview = self.ingest(
            Operation::SessionRewindPreview,
            Some(&session),
            None,
            Some(preview_body),
            None,
            None,
        )?;
        let selection = crate::wire::decode_rewind_preview(&preview)?;
        if selection.session != session || selection.prompt.len() > self.bounds.max_prompt_bytes {
            return Err(ClientError::Body {
                operation: Operation::SessionRewindPreview.operation_id(),
                reason: "selected prompt is foreign or exceeds the composer bound".into(),
            });
        }
        let body = crate::wire::encode_rewind(&selection)?;
        let committed = match self.ingest(
            Operation::SessionRewind,
            Some(&session),
            None,
            Some(body.clone()),
            None,
            None,
        ) {
            Ok(bytes) => bytes,
            Err(error) if submission::uncertain(&error) => {
                // One exact-source retry can reconcile the stable server operation;
                // it cannot select another prompt or remove another exchange.
                match self.ingest(
                    Operation::SessionRewind,
                    Some(&session),
                    None,
                    Some(body),
                    None,
                    None,
                ) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        self.rewind_uncertain = Some(session);
                        return Err(ClientError::RewindUnconfirmed);
                    }
                }
            }
            Err(error) => return Err(error),
        };
        if crate::wire::decode_rewound(&committed, &selection).is_err() {
            self.rewind_uncertain = Some(session);
            return Err(ClientError::RewindUnconfirmed);
        }
        // Drop buffered frames from the old conversation before rebuilding it.
        self.connection.cancel_read();
        self.recovery.history = None;
        self.reset_history_window();
        self.views.remove(&session);
        // The rebuilt conversation subscribes afresh, never as a resume.
        self.drop_resume();
        self.offer_resume();
        self.connection.resubscribe();
        let history = match self.reconcile(&session) {
            Ok(()) if self.views.contains_key(&session) => RewindHistory::Refreshed,
            Err(error) if !error.is_transient() => RewindHistory::Failed(error.to_string()),
            // A rewind may close the event stream. Its replacement can outlive
            // this command, and a positions read waits for it (or failed
            // transiently) with nothing rebuilt: the existing recovery owner
            // reloads the committed cut without replaying it or losing the draft.
            _ => {
                self.cursor.stream_closed();
                self.request_history();
                RewindHistory::Recovering
            }
        };
        Ok(RewindEdit {
            prompt: selection.prompt,
            history,
        })
    }
}
