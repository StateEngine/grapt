//! Native file undo keeps the exact preview across uncertain transport replies.
use super::*;

impl<C: Connection> Client<C> {
    pub fn visible_file_undo(&self) -> Option<&crate::wire::UndoSelection> {
        self.undo_preview.as_ref().filter(|preview| {
            self.undo_review_open && self.selected.as_ref() == Some(&preview.session)
        })
    }

    pub fn preview_file_undo(&mut self) -> Result<String, ClientError> {
        if self.undo_uncertain {
            return Err(ClientError::UndoUnconfirmed);
        }
        if self.rewind_uncertain.is_some() {
            return Err(ClientError::RewindUnconfirmed);
        }
        let session = self.selected.clone().ok_or(ClientError::NoSelection)?;
        if self.pending.contains_key(&session) {
            return Err(ClientError::Unconfirmed);
        }
        let bytes = self.ingest(
            Operation::SessionUndoPreview,
            Some(&session),
            None,
            Some(b"{}".to_vec()),
            None,
            None,
        )?;
        let selected = crate::wire::decode_undo_preview(&bytes)?;
        if selected.session != session {
            return Err(ClientError::Body {
                operation: Operation::SessionUndoPreview.operation_id(),
                reason: "file undo preview names another Session".into(),
            });
        }
        let coverage = if selected.incomplete_patches > 0 {
            format!(" {} partial patch calls: only confirmed applied operations are included; inspect other files.", selected.incomplete_patches)
        } else {
            String::new()
        };
        let summary = format!("File undo: {} writes, {} deletes. {}.{} Use /undo apply to restore these files. Conversation stays unchanged.", selected.writes, selected.deletes, selected.paths.join(", "), coverage);
        self.undo_preview = Some(selected);
        self.undo_review_open = true;
        Ok(summary)
    }

    pub fn apply_file_undo(&mut self) -> Result<crate::wire::UndoReceipt, ClientError> {
        let selected = self
            .undo_preview
            .as_ref()
            .ok_or_else(|| ClientError::Body {
                operation: Operation::SessionUndo.operation_id(),
                reason: "Use /undo to review the files first.".into(),
            })?;
        if self.selected.as_ref() != Some(&selected.session) {
            return Err(ClientError::Body {
                operation: Operation::SessionUndo.operation_id(),
                reason: "Select the Session whose file undo you reviewed.".into(),
            });
        }
        if self.pending.contains_key(&selected.session) {
            return Err(ClientError::Unconfirmed);
        }
        let body = crate::wire::encode_undo(selected)?;
        let session = selected.session.clone();
        // The server reconciles the checkpoint target through its existing
        // journal. One uncertain reply retries the identical reviewed target.
        let mut result = self.ingest(
            Operation::SessionUndo,
            Some(&session),
            None,
            Some(body.clone()),
            None,
            None,
        );
        if result.as_ref().is_err_and(submission::uncertain) {
            // A retry refusal does not settle the first request's effects.
            // Only a verified journal receipt can clear this uncertainty.
            self.undo_uncertain = true;
            result = self.ingest(
                Operation::SessionUndo,
                Some(&session),
                None,
                Some(body),
                None,
                None,
            );
        }
        let bytes = match result {
            Ok(bytes) => bytes,
            Err(error) => {
                if self.undo_uncertain || submission::uncertain(&error) {
                    self.undo_uncertain = true;
                    return Err(ClientError::UndoUnconfirmed);
                }
                return Err(error);
            }
        };
        let selected = self
            .undo_preview
            .as_ref()
            .ok_or(ClientError::UndoUnconfirmed)?;
        let receipt = match crate::wire::decode_undone(&bytes, selected) {
            Ok(receipt) => receipt,
            Err(_) => {
                self.undo_uncertain = true;
                return Err(ClientError::UndoUnconfirmed);
            }
        };
        self.undo_uncertain = false;
        self.undo_review_open = false;
        // Retain the immutable preview so another explicit apply can reconcile
        // the same terminal journal receipt; it cannot restore another turn.
        Ok(receipt)
    }
}
