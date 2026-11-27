//! Typed selection survives refresh; navigation and engine effects have separate owners.
// Copyright 2025 OpenAI. Apache-2.0.
// Modified for server title search, profile pins and acknowledged row management.
mod manage;
mod pins;
mod render;
use crate::backend::Agent;
use crossterm::event::KeyCode;
use grapt_client::wire::SessionWireId;
pub use manage::{handle, poll_search, Message, Request, Search};
pub use pins::Pins;
use ratatui_textarea::TextArea;
use std::time::Duration;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Group {
    #[default]
    Date,
    Directory,
}
enum Edit {
    Rename {
        id: SessionWireId,
        input: Box<TextArea<'static>>,
    },
    Delete {
        id: SessionWireId,
    },
}
pub struct Picker {
    query: String,
    selected: Option<SessionWireId>,
    offset: usize,
    page: usize,
    group: Group,
    changed: Duration,
    requested: Option<String>,
    search: Option<Search>,
    edit: Option<Edit>,
    pending: Option<Request>,
}
impl Picker {
    pub fn new(agents: &[Agent]) -> Self {
        Self {
            query: String::new(),
            selected: agents
                .iter()
                .find(|a| a.current)
                .or_else(|| agents.first())
                .map(|a| a.row.id.clone()),
            offset: 0,
            page: 10,
            group: Group::Date,
            changed: Duration::ZERO,
            requested: None,
            search: None,
            edit: None,
            pending: None,
        }
    }
    pub fn editing(&self) -> bool {
        self.edit.is_some() || self.pending.is_some()
    }
    fn source<'a>(&'a self, agents: &'a [Agent]) -> &'a [Agent] {
        match &self.search {
            Some(Search {
                query,
                result: Ok(rows),
            }) if query == &self.query => rows,
            _ => agents,
        }
    }
    fn entries<'a>(&'a self, agents: &'a [Agent], pins: &Pins) -> Vec<&'a Agent> {
        let query = self.query.to_lowercase();
        let mut rows: Vec<_> = self
            .source(agents)
            .iter()
            .filter(|a| a.row.title.to_lowercase().contains(&query))
            .collect();
        rows.sort_by(|a, b| {
            let pin = |a: &Agent| pins.position(&a.row.id).unwrap_or(usize::MAX);
            pin(a)
                .cmp(&pin(b))
                .then_with(|| match self.group {
                    Group::Directory => a.row.detail.directory.cmp(&b.row.detail.directory),
                    Group::Date => std::cmp::Ordering::Equal,
                })
                .then_with(|| b.row.detail.updated_ms.cmp(&a.row.detail.updated_ms))
                .then_with(|| a.row.id.cmp(&b.row.id))
        });
        rows
    }
    pub fn selected<'a>(&'a self, agents: &'a [Agent], pins: &Pins) -> Option<&'a SessionWireId> {
        self.entries(agents, pins)
            .into_iter()
            .find(|a| Some(&a.row.id) == self.selected.as_ref())
            .map(|a| &a.row.id)
    }
    pub fn key(&mut self, key: KeyCode, agents: &[Agent], pins: &Pins) {
        match key {
            KeyCode::Char(ch) if !ch.is_control() => {
                self.insert(&ch.to_string(), agents, pins);
                return;
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.filtered(agents, pins);
                return;
            }
            _ => {}
        }
        let entries = self.entries(agents, pins);
        let current = entries
            .iter()
            .position(|a| Some(&a.row.id) == self.selected.as_ref())
            .unwrap_or(0);
        let last = entries.len().saturating_sub(1);
        let next = match key {
            KeyCode::Up => current.saturating_sub(1),
            KeyCode::Down => current.saturating_add(1).min(last),
            KeyCode::PageUp => current.saturating_sub(self.page),
            KeyCode::PageDown => current.saturating_add(self.page).min(last),
            KeyCode::Home => 0,
            KeyCode::End => last,
            _ => return,
        };
        self.selected = entries.get(next).map(|a| a.row.id.clone());
    }
    pub fn insert(&mut self, text: &str, agents: &[Agent], pins: &Pins) -> bool {
        if let Some(Edit::Rename { input, .. }) = &mut self.edit {
            let text: String = text.chars().filter(|ch| !ch.is_control()).collect();
            let mut edited = input.clone();
            edited.insert_str(text);
            if edited.lines()[0].chars().count() > 100 {
                return false;
            }
            *input = edited;
            return true;
        }
        if self.edit.is_some() || self.pending.is_some() {
            return false;
        }
        if self.query.len().saturating_add(text.len()) > 256 {
            return false;
        }
        self.query
            .extend(text.chars().filter(|ch| !ch.is_control()));
        self.filtered(agents, pins);
        true
    }
    pub fn clear_filter(&mut self, agents: &[Agent], pins: &Pins) {
        self.query.clear();
        self.filtered(agents, pins);
    }
    fn filtered(&mut self, agents: &[Agent], pins: &Pins) {
        let selected = self
            .entries(agents, pins)
            .into_iter()
            .find(|a| Some(&a.row.id) == self.selected.as_ref())
            .or_else(|| self.entries(agents, pins).first().copied())
            .map(|a| a.row.id.clone());
        self.selected = selected;
        self.offset = 0;
    }
    pub fn schedule(&mut self, now: Duration) {
        self.changed = now;
        if self.query.is_empty() {
            self.search = None;
            self.requested = None;
        }
    }
    fn search_due(&self, now: Duration) -> Option<String> {
        (!self.query.is_empty()
            && self.requested.as_ref() != Some(&self.query)
            && now.saturating_sub(self.changed) >= Duration::from_millis(150))
        .then(|| self.query.clone())
    }
    fn accept_search(&mut self, search: Search, agents: &[Agent], pins: &Pins) {
        if search.query == self.query {
            self.search = Some(search);
            self.filtered(agents, pins);
        }
    }
    fn search_hint(&self) -> String {
        if self.query.is_empty() {
            return "Search titles across saved sessions".into();
        }
        match &self.search {
            Some(Search {
                query,
                result: Err(error),
            }) if query == &self.query => format!(
                "Search unavailable: {} · local matches",
                grapt_tui_transcript::markdown::literal(error)
            ),
            Some(Search {
                query,
                result: Ok(rows),
            }) if query == &self.query => {
                format!("Server results · {} matches (first page)", rows.len())
            }
            _ => "Searching… · showing local matches".into(),
        }
    }
}
#[cfg(test)]
mod tests;
