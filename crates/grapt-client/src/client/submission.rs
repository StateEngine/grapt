//! A lost reply retains the original submission until positively reconciled.
//! Editing the composer never replaces that identity or cancels admitted work.

use super::*;
use crate::wire::SubmissionOrigin;
use grapt_api::u3a::WireId;
use grapt_types::MessageId;

pub(super) struct PendingPrompt {
    text: String,
    files: Vec<grapt_api::u3a::PromptInputFileAttachmentDto>,
    id: WireId<MessageId>,
    origin: SubmissionOrigin,
}

impl std::fmt::Debug for PendingPrompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingPrompt")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl<C: Connection> Client<C> {
    /// Send a new prompt or an exact retry. An edit of an unresolved prompt
    /// only checks its admission; it never sends either text automatically.
    pub fn submit_prompt(&mut self, text: &str) -> Result<(), ClientError> {
        self.submit_prompt_with_files(text, &[])
    }

    /// Keep exact image bytes alongside the stable identity through uncertain delivery.
    pub fn submit_prompt_with_files(
        &mut self,
        text: &str,
        files: &[grapt_api::u3a::PromptInputFileAttachmentDto],
    ) -> Result<(), ClientError> {
        self.submit_with_origin(text, files, SubmissionOrigin::Composer)
    }

    /// A locally queued follow-up waits for any concurrently started execution.
    /// Its uncertain retry retains that delivery mode and never clears a composer.
    pub fn submit_queued_prompt(
        &mut self,
        text: &str,
        files: &[grapt_api::u3a::PromptInputFileAttachmentDto],
    ) -> Result<(), ClientError> {
        self.submit_with_origin(text, files, SubmissionOrigin::Followup)
    }

    /// Prepared-seat input uses explicit provenance, including uncertain retries.
    pub fn submit_seat_startup(&mut self, text: &str) -> Result<(), ClientError> {
        self.submit_with_origin(text, &[], SubmissionOrigin::SeatStartup)
    }

    fn submit_with_origin(
        &mut self,
        text: &str,
        files: &[grapt_api::u3a::PromptInputFileAttachmentDto],
        origin: SubmissionOrigin,
    ) -> Result<(), ClientError> {
        let session = self.selected.clone().ok_or(ClientError::NoSelection)?;
        if self.undo_uncertain {
            return Err(ClientError::UndoUnconfirmed);
        }
        self.undo_review_open = false;
        if self.rewind_uncertain.as_ref() == Some(&session) {
            return Err(ClientError::RewindUnconfirmed);
        }
        self.require_launch_preset()?;
        if self.launch_effort.is_some() {
            return Err(ClientError::NotOffered {
                what: "launch reasoning effort has not been applied",
            });
        }
        if !self.ready_to_chat() {
            return Err(ClientError::SelectionIncomplete);
        }
        self.recovered = false;
        if let Some(held) = self.pending.get(&session) {
            if held.text != text || held.files != files || held.origin != origin {
                if !held.files.is_empty() {
                    return Err(ClientError::UnconfirmedEdit);
                }
                let id = held.id.clone();
                let original = held.text.clone();
                let origin = held.origin;
                return if self.input_is_confirmed(&session, &id, &original, origin) {
                    self.pending.remove(&session);
                    Err(ClientError::EarlierSubmitted)
                } else {
                    Err(ClientError::UnconfirmedEdit)
                };
            }
        }
        let (prompt, body) = self.draft_input(&session, text, files, origin)?;
        match self.ingest(
            Operation::SessionPrompt,
            Some(&session),
            None,
            Some(body),
            None,
            None,
        ) {
            Ok(_) => {
                self.pending.remove(&session);
                Ok(())
            }
            Err(error) if uncertain(&error) => {
                let id = prompt.id.clone();
                let text_only = prompt.files.is_empty();
                // A failed retry cannot erase the original uncertain request.
                self.pending.entry(session.clone()).or_insert(prompt);
                if text_only && self.input_is_confirmed(&session, &id, text, origin) {
                    self.pending.remove(&session);
                    self.recovered = true;
                    Ok(())
                } else {
                    Err(ClientError::Unconfirmed)
                }
            }
            Err(error) => Err(error),
        }
    }

    /// Explicitly retry this session's original text under its original id.
    /// Only an original human input matching the composer may clear it.
    /// Automated retries preserve human drafts even when their text is equal.
    pub fn retry_prompt(&mut self, composer: &str) -> Result<bool, ClientError> {
        let session = self.selected.as_ref().ok_or(ClientError::NoSelection)?;
        let held = self
            .pending
            .get(session)
            .ok_or(ClientError::NoUnconfirmedPrompt)?;
        let original = held.text.clone();
        let files = held.files.clone();
        let origin = held.origin;
        self.submit_with_origin(&original, &files, origin)?;
        Ok(origin == SubmissionOrigin::Composer && original == composer)
    }

    pub fn has_unconfirmed_prompt(&self) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|session| self.pending.contains_key(session))
    }

    pub fn take_recovered(&mut self) -> bool {
        std::mem::take(&mut self.recovered)
    }

    pub(super) fn queued_inputs_unavailable(
        &mut self,
        session: &SessionWireId,
        error: ClientError,
    ) {
        if let Some(view) = self.views.get_mut(session) {
            view.queued.unavailable = Some(error.to_string().chars().take(512).collect());
        }
    }

    pub(super) fn install_queued_inputs(
        &mut self,
        session: &SessionWireId,
        bytes: &[u8],
    ) -> Result<(), ClientError> {
        let queued = crate::wire::queued_inputs(bytes, session)?;
        self.views
            .get_mut(session)
            .ok_or(ClientError::NoSelection)?
            .queued = queued;
        Ok(())
    }

    fn draft_input(
        &self,
        session: &SessionWireId,
        text: &str,
        files: &[grapt_api::u3a::PromptInputFileAttachmentDto],
        origin: SubmissionOrigin,
    ) -> Result<(PendingPrompt, Vec<u8>), ClientError> {
        let id = match self.pending.get(session) {
            Some(held) => held.id.clone(),
            None => {
                let ids = self
                    .input_ids
                    .as_ref()
                    .ok_or(ClientError::InputIdentityUnavailable)?;
                WireId::parse(format!("msg_{}", ids.mint().as_str()))
                    .ok_or(ClientError::InputIdentityUnavailable)?
            }
        };
        let body = crate::wire::encode_submission(text, &id, files, origin)?;
        let held_bytes = self
            .pending
            .iter()
            .filter(|(id, _)| *id != session)
            .map(|(_, held)| {
                held.text.len()
                    + held
                        .files
                        .iter()
                        .map(|file| file.uri().get().as_str().len())
                        .sum::<usize>()
            })
            .sum::<usize>();
        if held_bytes.saturating_add(body.len()) > 16 * 1024 * 1024 {
            return Err(ClientError::NotOffered {
                what: "unconfirmed drafts exceed 16 MiB; retry existing submissions first",
            });
        }

        if !self.pending.contains_key(session)
            && self.pending.len() >= self.bounds.max_retained_sessions
        {
            return Err(ClientError::UnconfirmedElsewhere {
                held: self.pending.len(),
            });
        }
        Ok((
            PendingPrompt {
                text: text.to_owned(),
                files: files.to_vec(),
                id,
                origin,
            },
            body,
        ))
    }

    /// Both reads are bounded and scoped to the addressed session. A positive
    /// pending read covers admission before promotion; a later history read
    /// covers promotion between reads. Missing, paginated, malformed or failed
    /// reads never prove rejection: the request may still be in flight.
    fn input_is_confirmed(
        &mut self,
        session: &SessionWireId,
        id: &WireId<MessageId>,
        text: &str,
        origin: SubmissionOrigin,
    ) -> bool {
        let pending = self.ingest(
            Operation::SessionPendingList,
            Some(session),
            None,
            None,
            None,
            None,
        );
        if pending.is_ok_and(|bytes| {
            crate::wire::pending_holds_input(&bytes, session, id, text, origin).unwrap_or(false)
        }) {
            return true;
        }
        self.ingest(
            Operation::MessageList,
            Some(session),
            None,
            None,
            None,
            Some(Page::First { order: "desc" }),
        )
        .is_ok_and(|bytes| {
            crate::wire::history_holds_input(&bytes, id, text, origin.startup()).unwrap_or(false)
        })
    }
}

pub(super) fn uncertain(error: &ClientError) -> bool {
    matches!(
        error,
        ClientError::Transport(
            TransportError::AfterSend { .. }
                | TransportError::ResponseTooLarge { .. }
                | TransportError::NotUtf8 { .. }
        ) | ClientError::Status {
            status: 408 | 500..=599,
            ..
        }
    )
}
