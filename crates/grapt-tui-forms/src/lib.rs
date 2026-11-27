//! Form drafts for session requests.
//! Copyright 2025 OpenAI. Apache-2.0; see licenses/openai-codex/
//! Modified by Grapt: canonical session forms, explicit settlement and bounded drafts.
mod ask;
mod choice_draw;
mod choices;
mod conditions;
mod defaults;
mod draft;
mod draw;
pub mod editor;
mod fields;
#[cfg(any(test, feature = "test-support"))]
mod observe;
mod target;
use draft::Draft;
use grapt_client::wire::{FormRow, FormState, FormValue, SessionWireId};
use std::collections::BTreeMap;
pub use target::Target;

pub const MAX_FORMS: usize = 16;
const MAX_FIELDS: usize = 32;
const MAX_FIELD: usize = 16_384;
const MAX_ANSWER: usize = 65_536;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Answer(BTreeMap<String, FormValue>),
    Cancel,
}
#[derive(Clone, Debug)]
pub struct Request {
    pub target: Target,
    pub form: FormRow,
    /// None is a read-only state check, including after an uncertain write.
    pub decision: Option<Decision>,
}
#[derive(Clone, Debug)]
pub struct Reply {
    pub target: Target,
    pub form: grapt_client::wire::FormWireId,
    pub state: Option<FormState>,
    /// A matching canonical FormInvalidAnswerError proves this decision was refused.
    pub validation_refused: bool,
    pub note: String,
}

#[derive(Default)]
pub struct Board {
    session: Option<SessionWireId>,
    location: Option<grapt_client::wire::FormLocation>,
    drafts: Vec<Draft>,
    selected: usize,
    pub focused: bool,
    omitted: usize,
}
impl std::fmt::Debug for Board {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Board")
            .field("drafts", &self.drafts.len())
            .field("selected", &self.selected)
            .field("focused", &self.focused)
            .finish_non_exhaustive()
    }
}

impl Board {
    pub fn count(&self) -> usize {
        self.drafts.len() + self.omitted
    }
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn update(
        &mut self,
        session: Option<SessionWireId>,
        location: Option<grapt_client::wire::FormLocation>,
        rows: Vec<FormRow>,
    ) -> bool {
        let selected = self
            .drafts
            .get(self.selected)
            .map(|draft| (draft.row.id.clone(), draft.target.clone()));
        let context_changed = self.session != session || self.location != location;
        self.drafts.retain(|draft| {
            draft
                .target
                .as_ref()
                .map(|target| target.active(session.as_ref(), location.as_ref()))
                .unwrap_or(!context_changed)
        });
        self.session = session;
        self.location = location;
        let old_rows: Vec<_> = self
            .drafts
            .iter()
            .map(|draft| (&draft.row, draft.listed))
            .collect();
        if !context_changed
            && self.omitted == rows.len().saturating_sub(MAX_FORMS)
            && old_rows.len() == rows.len().min(MAX_FORMS)
            && old_rows
                .iter()
                .zip(&rows)
                .all(|((old, listed), new)| *listed && *old == new)
        {
            return false;
        }
        let previous = std::mem::take(&mut self.drafts);
        for mut draft in previous {
            let current = rows.iter().find(|row| row.id == draft.row.id);
            let unresolved = draft.pending || draft.decision.is_some();
            match current {
                Some(row) if row == &draft.row => draft.listed = true,
                Some(row) if defaults::only_defaults_changed(&draft.row, row) => {
                    draft.row = row.clone();
                    draft.listed = true;
                }
                Some(_) if unresolved => {
                    draft.listed = false;
                    draft.note =
                        "Question fields changed; original decision kept. Ctrl+O checks state"
                            .into();
                }
                Some(row) => {
                    draft = Draft::new(
                        Target::for_row(row, self.session.as_ref(), self.location.as_ref()),
                        row.clone(),
                    );
                    draft.note = "Question fields changed; review them before answering".into();
                }
                None => {
                    if ask::is_ask(&draft.row) && !unresolved {
                        continue;
                    }
                    let selected_here = selected
                        .as_ref()
                        .is_some_and(|(id, target)| *id == draft.row.id && *target == draft.target);
                    let open = draft.state.as_ref().is_none_or(FormState::is_open);
                    if !(open && (unresolved || !draft.values.is_empty() || selected_here)
                        || selected_here && self.focused && rows.is_empty())
                    {
                        continue;
                    }
                    draft.listed = false;
                    if open {
                        draft.note =
                            "No longer outstanding; Ctrl+O checks its state. Draft kept".into();
                    }
                }
            }
            self.drafts.push(draft);
        }
        // Existing answer drafts own their bounded slots. Newly discovered
        // questions never evict an edited or unconfirmed question off-screen.
        let mut discovered = None;
        for row in &rows {
            if self.drafts.len() == MAX_FORMS {
                break;
            }
            if !self.drafts.iter().any(|draft| draft.row.id == row.id) {
                discovered.get_or_insert_with(|| row.id.clone());
                self.drafts.push(Draft::new(
                    Target::for_row(row, self.session.as_ref(), self.location.as_ref()),
                    row.clone(),
                ));
            }
        }
        self.omitted = rows
            .iter()
            .filter(|row| !self.drafts.iter().any(|draft| draft.row.id == row.id))
            .count();
        self.selected = selected
            .and_then(|(id, target)| {
                self.drafts
                    .iter()
                    .position(|draft| draft.row.id == id && draft.target == target)
            })
            .unwrap_or(0);
        if self
            .drafts
            .get(self.selected)
            .is_some_and(|draft| ask::is_ask(&draft.row) && !draft.listed)
        {
            self.focused = false;
        }
        if let Some(id) = discovered.filter(|_| !self.focused) {
            self.selected = self
                .drafts
                .iter()
                .position(|draft| draft.row.id == id)
                .unwrap_or(0);
            self.focused = true;
        }
        if self.drafts.is_empty() {
            self.focused = false;
        }
        true
    }
    pub fn key(&mut self, key: crossterm::event::KeyEvent) -> Option<Request> {
        use crossterm::event::{KeyCode, KeyModifiers};
        if key.code == KeyCode::Esc {
            if self.input_band() {
                return self
                    .drafts
                    .get_mut(self.selected)?
                    .key(crossterm::event::KeyEvent::new(
                        KeyCode::Char('r'),
                        KeyModifiers::CONTROL,
                    ));
            }
            self.focused = false;
            return None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('n' | 'p'))
        {
            let count = self.drafts.len();
            if count > 0 {
                self.selected = if key.code == KeyCode::Char('n') {
                    (self.selected + 1) % count
                } else {
                    (self.selected + count - 1) % count
                };
            }
            return None;
        }
        self.drafts.get_mut(self.selected)?.key(key)
    }
    pub fn paste(&mut self, text: &str) {
        if let Some(draft) = self.drafts.get_mut(self.selected) {
            draft.paste(text);
        }
    }
    pub fn send_failed(&mut self, note: String) {
        if let Some(draft) = self.drafts.get_mut(self.selected) {
            draft.pending = false;
            draft.note = note;
        }
    }
    pub fn reply(&mut self, reply: Reply) -> bool {
        if !reply
            .target
            .active(self.session.as_ref(), self.location.as_ref())
        {
            return false;
        }
        let selected = self
            .drafts
            .get(self.selected)
            .is_some_and(|draft| draft.row.id == reply.form);
        let Some(draft) = self.drafts.iter_mut().find(|draft| {
            draft.row.id == reply.form && draft.target.as_ref() == Some(&reply.target)
        }) else {
            return false;
        };
        draft.pending = false;
        draft.note = reply.note;
        if reply.state.is_some() {
            draft.state = reply.state;
        }
        if reply.validation_refused || draft.state.as_ref().is_some_and(|state| !state.is_open()) {
            draft.decision = None;
            if selected
                && ask::is_ask(&draft.row)
                && draft.state.as_ref().is_some_and(|s| !s.is_open())
            {
                self.focused = false;
            }
        }
        true
    }
}

pub(crate) fn supported(field: &grapt_client::wire::FormField) -> bool {
    fields::input(field).is_ok()
}

pub fn validate(row: &FormRow, values: &BTreeMap<String, FormValue>) -> Result<(), String> {
    if row.fields.len() > MAX_FIELDS {
        return Err("Too many fields for R; question remains unresolved".into());
    }
    let (visible, active) = conditions::project(row, values)?;
    if active.len() != values.len() {
        return Err("Answer contains inactive or unknown fields".into());
    }
    let mut keys = std::collections::BTreeSet::new();
    let mut bytes = 13usize; // Include the JSON object wrapper and entry separators.
    for (index, field) in row.fields.iter().enumerate() {
        if !keys.insert(&field.key) || !supported(field) {
            return Err(format!(
                "Unsupported field: {}; answer not sent",
                grapt_tui_transcript::markdown::literal(&field.key)
            ));
        }
        if !visible.contains(&index) {
            continue;
        }
        match values.get(&field.key) {
            Some(value) => {
                fields::validate_value(field, value)?;
                bytes += serde_json::Value::String(field.key.clone())
                    .to_string()
                    .len()
                    + value.to_json().to_string().len()
                    + 2;
                if bytes > MAX_ANSWER {
                    return Err("Answer exceeds its 64 KiB bound".into());
                }
            }
            None if !field.required => {}
            None => {
                return Err(format!(
                    "Still needed: {}",
                    grapt_tui_transcript::markdown::literal(&field.key)
                ))
            }
        }
    }
    if values.keys().any(|key| !keys.contains(key)) || bytes > MAX_ANSWER {
        return Err("Answer exceeds its fields or 64 KiB bound".into());
    }
    Ok(())
}

#[cfg(test)]
mod inputs_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod location_tests;

#[cfg(test)]
mod defaults_tests;

#[cfg(test)]
mod constraints_tests;
