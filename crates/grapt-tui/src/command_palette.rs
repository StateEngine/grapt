// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// at bb64b894fd67158be0f874872e6d7bfe717e7368. Modified for existing native
// actions, truthful availability and explicit command staging.
use crate::{
    actions::{self, Action},
    ui::Ui,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use grapt_tui_transcript::{markdown::literal, palette::Palette};
use ratatui::{
    layout::Rect,
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

#[derive(Clone)]
pub enum Target {
    Action(Action),
    Command(String),
}
struct Entry {
    title: String,
    detail: String,
    target: Target,
    disabled: Option<&'static str>,
}
#[derive(Default)]
pub struct Picker {
    query: String,
    cursor: usize,
    page: usize,
    offset: usize,
}
fn entries(ui: &Ui, query: &str) -> Vec<Entry> {
    let mut rows: Vec<_> = actions::SPECS
        .iter()
        .filter(|s| s.action != Action::Palette && !matches!(s.action, Action::SessionSlot(slot) if ui.session_pins.slot(slot).is_none()))
        .map(|s| Entry {
            title: s.title.into(),
            detail: format!(
                "{} · {} {}",
                actions::group(s.action),
                ui.bindings.label(s.action),
                actions::names(s)
            ),
            target: Target::Action(s.action),
            disabled: actions::reason(s.action, ui),
        })
        .collect();
    let has_draft = !crate::keys::draft(ui).is_empty() || ui.images.len() > 0;
    let mut names = std::collections::BTreeSet::new();
    for (name, description) in crate::slash::BUILTINS
        .iter()
        .copied()
        .chain(ui.slash.commands())
    {
        if actions::SPECS.iter().any(|spec| spec.slash == Some(name))
            || actions::slash(&format!("/{name}")).is_some()
            || name == "clear"
            || !names.insert(name)
        {
            continue;
        }
        rows.push(Entry {
            title: format!("/{name}"),
            detail: description.into(),
            target: Target::Command(format!("/{name} ")),
            disabled: has_draft.then_some("Keep or clear the current draft first"),
        });
        if rows.len() >= 256 {
            break;
        }
    }
    let query = query.trim().to_lowercase();
    rows.into_iter()
        .filter(|row| {
            let text = format!("{} {}", row.title, row.detail).to_lowercase();
            query.split_whitespace().all(|word| text.contains(word))
        })
        .collect()
}
impl Picker {
    pub fn paste(&mut self, text: &str) {
        if self.query.len().saturating_add(text.len()) <= 256 {
            self.query.extend(text.chars().filter(|c| !c.is_control()));
            self.cursor = 0;
            self.offset = 0;
        }
    }
    fn key(&mut self, key: KeyEvent, ui: &Ui) -> Option<Target> {
        let rows = entries(ui, &self.query);
        self.cursor = self.cursor.min(rows.len().saturating_sub(1));
        match key.code {
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down => self.cursor = (self.cursor + 1).min(rows.len().saturating_sub(1)),
            KeyCode::PageUp => self.cursor = self.cursor.saturating_sub(self.page.max(1)),
            KeyCode::PageDown => {
                self.cursor = (self.cursor + self.page.max(1)).min(rows.len().saturating_sub(1))
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = rows.len().saturating_sub(1),
            KeyCode::Enter => {
                return rows
                    .get(self.cursor)
                    .filter(|r| r.disabled.is_none())
                    .map(|r| r.target.clone())
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.cursor = 0;
                self.offset = 0;
            }
            KeyCode::Char(ch)
                if !ch.is_control()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.paste(&ch.to_string())
            }
            _ => {}
        }
        None
    }
    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, p: Palette, ui: &Ui) {
        let rows = entries(ui, &self.query);
        self.cursor = self.cursor.min(rows.len().saturating_sub(1));
        frame.render_widget(Clear, area);
        let close_hint = ui
            .bindings
            .shortcut(Action::Palette)
            .map_or_else(|| "Esc closes".into(), |key| format!("{key} / Esc closes"));
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" Commands · {close_hint} "))
            .style(p.normal().bg(p.panel));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        self.page = usize::from(inner.height).saturating_sub(5).max(1);
        if self.cursor < self.offset {
            self.offset = self.cursor;
        }
        if self.cursor >= self.offset + self.page {
            self.offset = self.cursor.saturating_add(1).saturating_sub(self.page);
        }
        let mut lines = vec![Line::from(format!("Search: {}", literal(&self.query)))];
        for (i, row) in rows.iter().enumerate().skip(self.offset).take(self.page) {
            let label = format!(
                "{}{}  {}",
                if i == self.cursor { "› " } else { "  " },
                row.title,
                row.detail
            );
            let style = p
                .normal()
                .fg(if i == self.cursor { p.accent } else { p.text });
            lines.push(Line::from(literal(&label)).style(style));
        }
        if rows.is_empty() {
            lines.push(Line::from("No matching commands"));
        }
        if let Some(reason) = rows.get(self.cursor).and_then(|r| r.disabled) {
            lines.push(Line::from(reason).style(p.normal().fg(p.muted)));
        }
        lines.push(Line::from(
            "↑/↓ choose · Enter selects · commands insert into an empty draft",
        ));
        lines.push(Line::from(format!(
            "/commands reloads workspace commands · {} help",
            ui.bindings
                .shortcut(Action::Help)
                .unwrap_or_else(|| "/help".into())
        )));
        frame.render_widget(Paragraph::new(lines), inner);
    }
}
pub fn key(key: KeyEvent, action: Option<Action>, ui: &mut Ui) -> Option<Target> {
    let mut picker = ui.command_palette.take()?;
    if key.code == KeyCode::Esc || action == Some(Action::Palette) {
        return None;
    }
    if action == Some(Action::Help) {
        return Some(Target::Action(Action::Help));
    }
    let target = picker.key(key, ui);
    if target.is_none() {
        ui.command_palette = Some(picker);
    }
    target
}
pub fn draw(ui: &mut Ui, frame: &mut Frame<'_>, area: Rect) {
    if let Some(mut picker) = ui.command_palette.take() {
        picker.draw(frame, area, ui.palette, ui);
        ui.command_palette = Some(picker);
    }
}

#[cfg(test)]
mod tests;
