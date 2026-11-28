//! Human submissions in flight, and the ones typed while one is in flight.
//!
//! Enter takes the text out of the composer at once, so typing during the
//! round trip starts a new draft instead of extending the one being sent, and
//! accepted text is never left where Enter could send it again. Enter while a
//! submission is in flight queues the new text behind it; the queue sends in
//! order as each submission is confirmed. A refusal holds the queue with the
//! refused text at its head; Enter on an empty draft sends it again.
//!
//! The Tab follow-up queue is a different contract: it waits for the running
//! work to finish successfully, while text queued here is sent the moment the
//! previous submission is confirmed, exactly as an immediate Enter would be.
use crate::{backend::Command, keys, ui::Ui, Backend};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use grapt_client::wire::SessionWireId;
use std::collections::VecDeque;

const MAX_WAITING: usize = 16;
const MAX_BYTES: usize = 16 * 1024 * 1024;

struct Waiting {
    session: Option<SessionWireId>,
    text: String,
}

#[derive(Default)]
pub struct Sending {
    /// The text in flight and the Session it was typed for.
    in_flight: Option<(String, Option<SessionWireId>)>,
    waiting: VecDeque<Waiting>,
    held: bool,
    selected: Option<SessionWireId>,
    revision: u64,
}

impl Sending {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn waiting_count(&self) -> usize {
        self.waiting.len()
    }

    /// Something is in flight or waiting, so new text must go behind it.
    pub fn busy(&self) -> bool {
        self.in_flight.is_some() || !self.waiting.is_empty()
    }

    /// The held head entry belongs to a Session other than the selected one.
    fn held_elsewhere(&self) -> bool {
        self.held
            && self
                .waiting
                .front()
                .is_some_and(|entry| entry.session != self.selected)
    }

    /// Removes the held head entry, for a refusal that resending cannot cure.
    fn take_held(&mut self) -> Option<String> {
        if !self.held {
            return None;
        }
        let entry = self.waiting.pop_front()?;
        if self.waiting.is_empty() {
            self.held = false;
        }
        self.revision = self.revision.wrapping_add(1);
        Some(entry.text)
    }

    pub fn observe(&mut self, selected: Option<SessionWireId>) {
        if self.selected != selected {
            self.selected = selected;
            self.revision = self.revision.wrapping_add(1);
        }
    }

    fn start(&mut self, text: String) {
        self.in_flight = Some((text, self.selected.clone()));
        self.revision = self.revision.wrapping_add(1);
    }

    fn queue(&mut self, text: String) -> Result<(), &'static str> {
        // The in-flight text counts: a refusal puts it back in the queue,
        // which must still fit the bound.
        let in_flight = self.in_flight.as_ref().map(|(text, _)| text.len());
        let entries = self.waiting.len() + usize::from(in_flight.is_some());
        let held: usize = self
            .waiting
            .iter()
            .map(|entry| entry.text.len())
            .sum::<usize>()
            + in_flight.unwrap_or(0);
        if entries >= MAX_WAITING || held.saturating_add(text.len()) > MAX_BYTES {
            return Err("Send queue is full (16 prompts / 16 MiB); draft kept");
        }
        self.waiting.push_back(Waiting {
            session: self.selected.clone(),
            text,
        });
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    /// Records the outcome of `text`; returns it when this queue sent it and
    /// the service refused it, so the caller can give it back.
    fn settle(&mut self, text: &str, accepted: bool) -> Option<Waiting> {
        if self.in_flight.as_ref().map(|(sent, _)| sent.as_str()) != Some(text) {
            return None;
        }
        let sent = self.in_flight.take();
        self.revision = self.revision.wrapping_add(1);
        if accepted {
            return None;
        }
        self.held = true;
        sent.map(|(text, session)| Waiting { session, text })
    }

    /// Puts refused text back at the head of the queue, held, still bound to
    /// the Session it was typed for.
    fn requeue(&mut self, entry: Waiting) {
        self.waiting.push_front(entry);
        self.held = true;
        self.revision = self.revision.wrapping_add(1);
    }

    /// The next text to send, when nothing is in flight and the queue is not held.
    fn next(&mut self) -> Option<String> {
        if self.held || self.in_flight.is_some() {
            return None;
        }
        let entry = self.waiting.front()?;
        if entry.session != self.selected {
            // Text typed for one Session never goes to another. A selection
            // not yet known again, as during a resync, is waited out; only a
            // different Session holds the queue.
            if self.selected.is_some() {
                self.held = true;
                self.revision = self.revision.wrapping_add(1);
            }
            return None;
        }
        let entry = self.waiting.pop_front()?;
        self.revision = self.revision.wrapping_add(1);
        Some(entry.text)
    }

    fn resume(&mut self) -> bool {
        if !self.held || self.waiting.is_empty() || self.held_elsewhere() {
            return false;
        }
        self.held = false;
        self.revision = self.revision.wrapping_add(1);
        true
    }

    pub fn lines(&self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some((text, _)) = &self.in_flight {
            lines.push(format!("Sending · {}", one_line(text)));
        }
        if !self.waiting.is_empty() {
            let state = if self.held_elsewhere() {
                "held for another Session · switch back to send · Alt+Delete discards it"
            } else if self.held {
                "held · Enter on an empty draft sends it · Alt+↑ edits · Alt+Delete discards · both act before follow-ups"
            } else {
                "sends after the one before is confirmed"
            };
            lines.push(format!(
                "Waiting to send ({}) · {state}",
                self.waiting.len()
            ));
            for entry in self.waiting.iter().take(2) {
                lines.push(one_line(&entry.text));
            }
        }
        lines
            .into_iter()
            .map(|line| grapt_client::cells::cut(&line, width))
            .collect()
    }
}

fn one_line(text: &str) -> String {
    grapt_tui_transcript::markdown::literal(text).replace('\n', " ")
}

/// Enter on a plain-text draft: send it now, or queue it behind the one in flight.
pub fn enter(ui: &mut Ui, backend: &Backend, text: String) {
    if text.trim().is_empty() {
        if ui.sending.resume() {
            ui.status = "Sending held drafts in order".into();
        } else if ui.sending.held_elsewhere() {
            ui.status = "The held draft belongs to another Session; switch back to send it, \
                         or Alt+Delete discards it"
                .into();
        }
        return;
    }
    // Composer commands complete into their own draft, so they keep it in
    // place until the command settles; only prompts leave the composer.
    let prompt =
        !text.trim_start().starts_with('/') && crate::backend::commands::submits_prompt(&text);
    // Anything in flight or waiting goes first: new text never overtakes it,
    // or a refused earlier prompt could be resent after a later one.
    if ui.pending.is_some() || ui.sending.busy() {
        if !prompt {
            ui.status = "Wait for the current submission to run a command; draft kept".into();
            return;
        }
        match ui.sending.queue(text.clone()) {
            Ok(()) => {
                ui.recall.replace_if_unchanged(&text, "", &mut ui.composer);
                ui.status = if ui.sending.held {
                    "Queued behind a held draft; Enter on an empty draft sends them in order"
                } else {
                    "Queued; sends after the current submission is confirmed"
                }
                .into();
            }
            Err(error) => ui.status = error.into(),
        }
        return;
    }
    match crate::send_tracked(backend, Command::Submit(text.clone())) {
        Ok(id) if prompt => {
            ui.navigation
                .jumped(grapt_client::client::HistoryMove::Latest, id);
            ui.recall.replace_if_unchanged(&text, "", &mut ui.composer);
            ui.sending.start(text.clone());
            ui.pending = Some(text);
            ui.status = "Submitting".into();
        }
        Ok(_) => {
            ui.pending = Some(text);
            ui.status = "Running command; draft kept until it completes".into();
        }
        Err(error) => ui.status = error,
    }
}

/// Alt+Delete discards the held head entry; Alt+Up returns it to an empty
/// composer for editing. True when the key acted on the held queue.
pub fn key(key: KeyEvent, ui: &mut Ui) -> bool {
    if key.modifiers != KeyModifiers::ALT
        || !matches!(key.code, KeyCode::Up | KeyCode::Delete)
        || !ui.sending.held
    {
        return false;
    }
    if key.code == KeyCode::Up && ui.sending.held_elsewhere() {
        ui.status = "The held draft belongs to another Session; switch back to edit it".into();
        return true;
    }
    if key.code == KeyCode::Up && (!keys::draft(ui).is_empty() || ui.images.len() > 0) {
        ui.status = "Keep or clear the current draft before editing a held one".into();
        return true;
    }
    let Some(text) = ui.sending.take_held() else {
        return false;
    };
    if key.code == KeyCode::Up {
        ui.composer.insert_str(text);
        ui.recall.edited();
        ui.status = "Held draft returned to the composer; Enter sends it".into();
    } else {
        ui.status = match ui.sending.waiting_count() {
            0 => "Held draft discarded".into(),
            more => format!(
                "Held draft discarded; {more} more held · Enter on an empty draft sends them"
            ),
        };
    }
    true
}

/// A submission outcome reached the UI, delivered once. Accepted text leaves
/// recall's draft; refused text waits, held, at the head of the queue. It
/// never returns into the composer, where new typing would extend it into a
/// different message. `current` is false when the view has moved on, which
/// must still settle the outcome or Enter would wait on it forever. True when
/// the screen changed.
pub fn submission(
    ui: &mut Ui,
    (text, accepted, human): (String, bool, bool),
    current: bool,
) -> bool {
    ui.pending = None;
    let dirty = accepted
        && human
        && if current {
            ui.submission_accepted(&text)
        } else {
            ui.recall.submitted(&text, &mut ui.composer)
        };
    if let Some(refused) = ui.sending.settle(&text, accepted) {
        ui.sending.requeue(refused);
    }
    dirty
}

/// Sends the next queued text once the previous submission is confirmed.
/// None is sent while a quit waits: it resumes only if the quit is refused.
pub fn advance(ui: &mut Ui, backend: &Backend) -> bool {
    if ui.pending.is_some() || !ui.launch.readiness.can_submit() || ui.quit.waiting() {
        return false;
    }
    let Some(text) = ui.sending.next() else {
        return false;
    };
    // Only prompts are queued, and each moves the view to the newest page.
    match crate::send_tracked(backend, Command::Submit(text.clone())) {
        Ok(id) => {
            ui.navigation
                .jumped(grapt_client::client::HistoryMove::Latest, id);
            ui.sending.start(text.clone());
            ui.pending = Some(text);
            ui.status = "Submitting queued draft".into();
        }
        Err(error) => {
            let session = ui.sending.selected.clone();
            ui.sending.requeue(Waiting { session, text });
            ui.status = error;
        }
    }
    true
}
