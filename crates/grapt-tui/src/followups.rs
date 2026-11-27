//! Local follow-ups restore pending input.
// Copyright 2025 OpenAI. Modified for bounded Session ownership and explicit custody.
use crate::{
    backend::{Command, CommandId},
    images::Settlement,
    keys, send_tracked,
    ui::Ui,
    Backend,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use grapt_api::u3a::PromptInputFileAttachmentDto;
use grapt_client::{
    wire::{MessageWireId, SessionWireId},
    Outcome,
};
use std::collections::VecDeque;

const MAX_QUEUED: usize = 16;
const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Request {
    pub session: SessionWireId,
    pub text: String,
    pub files: Vec<PromptInputFileAttachmentDto>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Custody {
    Accepted,
    Unconfirmed,
    Refused,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    Sending,
    Unconfirmed,
    Paused,
}

/// A queued draft's place in this window's queue, for the local operations an
/// unsent draft allows. It never travels with a command and converts to and
/// from nothing, so it cannot correlate a result: a sent draft is matched only
/// by the `CommandId` its send returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DraftKey(u64);

struct Draft {
    key: DraftKey,
    /// The command that carried this draft, once it was sent.
    sent: Option<CommandId>,
    request: Request,
    state: State,
    after: Option<MessageWireId>,
}

#[derive(Default)]
pub struct Queue {
    entries: VecDeque<Draft>,
    selected: Option<SessionWireId>,
    completion: Option<(MessageWireId, Outcome)>,
    active_after: Option<MessageWireId>,
    connected: bool,
    observed_running: Option<bool>,
    serial: u64,
    revision: u64,
}

impl Queue {
    pub fn pending_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|draft| {
                Some(&draft.request.session) == self.selected.as_ref()
                    && matches!(draft.state, State::Ready | State::Paused)
            })
            .count()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn disconnected(&mut self) {
        self.connected = false;
        self.observed_running = None;
    }

    pub fn observe(
        &mut self,
        selected: Option<SessionWireId>,
        completion: Option<(MessageWireId, Outcome)>,
        connected: bool,
        observed_running: Option<bool>,
    ) {
        let same_session = self.selected == selected;
        if !same_session {
            self.pause();
        }
        if observed_running == Some(true) && (!same_session || self.observed_running != Some(true))
        {
            self.active_after = if same_session {
                self.completion.as_ref().map(|(id, _)| id.clone())
            } else {
                completion.as_ref().map(|(id, _)| id.clone())
            };
        }
        self.selected = selected;
        self.completion = completion;
        self.connected = connected;
        self.observed_running = observed_running;
        if connected && observed_running == Some(false) {
            if let Some((id, outcome)) = &self.completion {
                for draft in &mut self.entries {
                    if Some(&draft.request.session) == self.selected.as_ref()
                        && draft.state == State::Ready
                        && draft.after.as_ref() != Some(id)
                        && *outcome != Outcome::Succeeded
                    {
                        draft.state = State::Paused;
                        self.revision = self.revision.wrapping_add(1);
                    }
                }
            }
        }
    }

    pub fn pause(&mut self) {
        for draft in &mut self.entries {
            if Some(&draft.request.session) == self.selected.as_ref() && draft.state == State::Ready
            {
                draft.state = State::Paused;
                self.revision = self.revision.wrapping_add(1);
            }
        }
    }

    fn enqueue(
        &mut self,
        text: String,
        files: Vec<PromptInputFileAttachmentDto>,
    ) -> Result<(), &'static str> {
        let session = self
            .selected
            .clone()
            .ok_or("No Session selected; draft kept")?;
        if self.observed_running != Some(true) {
            return Err("Tab queues a follow-up while work is running; Enter sends now");
        }
        if !self.connected {
            return Err("Connection unavailable; draft kept");
        }
        let size = |text: &str, files: &[PromptInputFileAttachmentDto]| {
            text.len().saturating_add(
                files
                    .iter()
                    .map(|file| file.uri().get().as_str().len())
                    .sum::<usize>(),
            )
        };
        let held = self
            .entries
            .iter()
            .map(|draft| size(&draft.request.text, &draft.request.files))
            .sum::<usize>();
        if self.entries.len() == MAX_QUEUED || held.saturating_add(size(&text, &files)) > MAX_BYTES
        {
            return Err("Follow-up queue is full (16 prompts / 16 MiB); draft kept");
        }
        self.serial = self
            .serial
            .checked_add(1)
            .ok_or("Follow-up identity exhausted; draft kept")?;
        self.entries.push_back(Draft {
            key: DraftKey(self.serial),
            sent: None,
            request: Request {
                session,
                text,
                files,
            },
            state: State::Ready,
            after: self.active_after.clone(),
        });
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    fn latest(&self) -> Option<&Draft> {
        self.entries
            .iter()
            .rev()
            .find(|draft| Some(&draft.request.session) == self.selected.as_ref())
    }

    fn remove(&mut self, key: DraftKey) {
        self.entries.retain(|draft| draft.key != key);
        self.revision = self.revision.wrapping_add(1);
    }

    /// The draft `key` went out as command `id`.
    fn sent(&mut self, key: DraftKey, id: CommandId) {
        if let Some(draft) = self.entries.iter_mut().find(|draft| draft.key == key) {
            draft.sent = Some(id);
        }
    }

    /// The draft `key` could not be handed to the backend: hold it paused.
    fn refuse(&mut self, key: DraftKey) {
        if let Some(draft) = self.entries.iter_mut().find(|draft| draft.key == key) {
            draft.state = State::Paused;
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// Settle the draft sent as command `id`; nothing else can match.
    pub fn acknowledge(&mut self, id: CommandId, custody: Custody) -> bool {
        let Some(draft) = self.entries.iter_mut().find(|draft| draft.sent == Some(id)) else {
            return false;
        };
        let key = draft.key;
        match custody {
            Custody::Accepted => self.remove(key),
            Custody::Unconfirmed => {
                draft.state = State::Unconfirmed;
                self.revision = self.revision.wrapping_add(1);
            }
            Custody::Refused => {
                draft.state = State::Paused;
                self.revision = self.revision.wrapping_add(1);
            }
        }
        true
    }

    fn next(&mut self) -> Option<(DraftKey, Request)> {
        if !self.connected
            || self
                .entries
                .iter()
                .any(|draft| matches!(draft.state, State::Sending | State::Unconfirmed))
        {
            return None;
        }
        if self.observed_running != Some(false) {
            return None;
        }
        let Some((completed, Outcome::Succeeded)) = &self.completion else {
            return None;
        };
        let index = self
            .entries
            .iter()
            .position(|draft| Some(&draft.request.session) == self.selected.as_ref())?;
        let draft = self.entries.get(index)?;
        if draft.state != State::Ready || draft.after.as_ref() == Some(completed) {
            return None;
        }
        // Durable message identity survives a history refresh and also covers
        // work that completes before a running frame reaches the UI.
        for draft in &mut self.entries {
            if Some(&draft.request.session) == self.selected.as_ref() && draft.state == State::Ready
            {
                draft.after = Some(completed.clone());
            }
        }
        let draft = self.entries.get_mut(index)?;
        draft.state = State::Sending;
        self.revision = self.revision.wrapping_add(1);
        Some((draft.key, draft.request.clone()))
    }

    pub fn lines(&self, width: usize) -> Vec<String> {
        let selected: Vec<_> = self
            .entries
            .iter()
            .filter(|draft| Some(&draft.request.session) == self.selected.as_ref())
            .collect();
        if selected.is_empty() {
            return Vec::new();
        }
        let paused = selected.iter().any(|draft| draft.state == State::Paused);
        let uncertain = selected
            .iter()
            .any(|draft| draft.state == State::Unconfirmed);
        let action = if uncertain {
            "unconfirmed · Alt+R checks original"
        } else if paused {
            "paused · Alt+↑ edit · Alt+Delete cancel"
        } else {
            "Alt+↑ edit · Alt+Delete cancel"
        };
        let mut lines = vec![format!("Queued follow-ups ({}) · {action}", selected.len())];
        for draft in selected.iter().rev().take(2).rev() {
            let text =
                grapt_tui_transcript::markdown::literal(&draft.request.text).replace('\n', " ");
            lines.push(if draft.request.files.is_empty() {
                text
            } else {
                format!("{text} [{} images]", draft.request.files.len())
            });
        }
        lines
            .into_iter()
            .map(|line| grapt_client::cells::cut(&line, width))
            .collect()
    }
}

pub fn key(key: KeyEvent, ui: &mut Ui) -> bool {
    if key.code == KeyCode::Tab && key.modifiers.is_empty() {
        if !ui.running() {
            return false;
        }
        let text = keys::draft(ui);
        if !ui.prompt_ready || ui.pending.is_some() || ui.views.loading() {
            ui.status = "Wait for the current submission; draft kept".into();
        } else if text.trim_start().starts_with('/') {
            ui.status = "Enter runs slash commands; Tab queues a plain prompt".into();
        } else if !text.is_empty() && !grapt_tui_transcript::prompt::valid_prompt(&text) {
            ui.status =
                "Prompt is empty, too large or contains unsupported controls; draft kept".into();
        } else if !text.trim().is_empty() || ui.images.len() > 0 {
            match ui.images.submission(text.clone()) {
                Ok(prompt) => match ui.followups.enqueue(text.clone(), prompt.files) {
                    Ok(()) => {
                        ui.images.settle_local(Settlement::Accepted);
                        ui.recall.replace_if_unchanged(&text, "", &mut ui.composer);
                        ui.status = "Follow-up queued; current work continues".into();
                    }
                    Err(error) => {
                        ui.images.settle_local(Settlement::Rejected);
                        ui.status = error.into();
                    }
                },
                Err(error) => ui.status = error.into(),
            }
        }
        return true;
    }
    if key.modifiers != KeyModifiers::ALT || !matches!(key.code, KeyCode::Up | KeyCode::Delete) {
        return false;
    }
    let Some(draft) = ui.followups.latest() else {
        return false;
    };
    if matches!(draft.state, State::Sending | State::Unconfirmed) {
        ui.status = "Submission started; use Alt+R to resolve an unconfirmed follow-up".into();
        return true;
    }
    let (draft_key, request) = (draft.key, draft.request.clone());
    if key.code == KeyCode::Up {
        if !keys::draft(ui).is_empty() || ui.images.len() > 0 || ui.pending.is_some() {
            ui.status = "Keep or clear the current draft before editing a queued follow-up".into();
            return true;
        }
        if let Err(error) = ui.images.restore(request.files) {
            ui.status = error.into();
            return true;
        }
        ui.composer.insert_str(request.text);
        ui.recall.edited();
        ui.status = "Follow-up returned to the draft; Tab queues it again".into();
    } else {
        ui.status = "Queued follow-up cancelled".into();
    }
    ui.followups.remove(draft_key);
    true
}

pub fn advance(ui: &mut Ui, backend: &Backend) -> bool {
    if !ui.prompt_ready || ui.pending.is_some() || ui.views.loading() || ui.quit.waiting() {
        return false;
    }
    let Some((key, request)) = ui.followups.next() else {
        return false;
    };
    match send_tracked(backend, Command::Followup(request)) {
        Ok(id) => ui.followups.sent(key, id),
        Err(error) => {
            ui.followups.refuse(key);
            ui.status = error;
        }
    }
    true
}

#[cfg(test)]
mod tests;
