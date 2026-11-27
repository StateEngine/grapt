// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// at bb64b894fd67158be0f874872e6d7bfe717e7368. Modified for typed native
// settings, bounded search and immediate effects only after a successful save.
mod preferences;
use crate::ui::Ui;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use grapt_tui_transcript::palette::Palette;
pub use preferences::{Preferences, Values};
use ratatui::{
    layout::Rect,
    text::Line,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};
#[derive(Clone, Copy)]
enum Setting {
    Theme,
    Notifications,
    Copy,
    Wheel,
}
impl Setting {
    fn title(self) -> &'static str {
        match self {
            Self::Theme => "Color mode",
            Self::Notifications => "Notifications",
            Self::Copy => "Copy on selection",
            Self::Wheel => "Mouse wheel speed",
        }
    }
    fn description(self) -> &'static str {
        match self {
            Self::Theme => "Appearance · light or dark theme",
            Self::Notifications => "Attention · notify when unfocused or idle",
            Self::Copy => "Mouse · copy transcript text after dragging",
            Self::Wheel => "Mouse · transcript rows per wheel tick (1–12)",
        }
    }
    fn value(self, v: Values) -> String {
        match self {
            Self::Theme => if v.light { "Light" } else { "Dark" }.into(),
            Self::Notifications => on_off(v.notifications).into(),
            Self::Copy => on_off(v.copy_on_select).into(),
            Self::Wheel => format!("{} rows", v.wheel_rows),
        }
    }
    fn change(self, v: &mut Values, backwards: bool) {
        match self {
            Self::Theme => v.light = !v.light,
            Self::Notifications => v.notifications = !v.notifications,
            Self::Copy => v.copy_on_select = !v.copy_on_select,
            Self::Wheel => {
                v.wheel_rows = if backwards {
                    v.wheel_rows.saturating_sub(1).max(1)
                } else {
                    v.wheel_rows.saturating_add(1).min(12)
                }
            }
        }
    }
}
fn on_off(on: bool) -> &'static str {
    if on {
        "On"
    } else {
        "Off"
    }
}
enum Notice {
    Saved(&'static str),
    Failed(String),
}
#[derive(Default)]
pub struct Picker {
    query: String,
    cursor: usize,
    notice: Option<Notice>,
}
impl Picker {
    fn entries(&self) -> Vec<Setting> {
        let query = self.query.to_lowercase();
        [
            Setting::Theme,
            Setting::Notifications,
            Setting::Copy,
            Setting::Wheel,
        ]
        .into_iter()
        .filter(|s| {
            format!("{} {}", s.title(), s.description())
                .to_lowercase()
                .contains(&query)
        })
        .collect()
    }
    pub fn paste(&mut self, text: &str) {
        for ch in text.chars().filter(|ch| !ch.is_control()) {
            if self.query.len() + ch.len_utf8() > 128 {
                break;
            }
            self.query.push(ch);
        }
        self.cursor = 0;
    }
    pub fn draw(&self, frame: &mut Frame<'_>, area: Rect, values: Values, p: Palette) {
        frame.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Settings · Enter/→ change · ← decrease · Esc close ")
            .style(p.normal().bg(p.panel));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height < 3 {
            return;
        }
        frame.render_widget(
            Paragraph::new(format!("Search: {}", self.query)).style(p.normal().fg(p.muted)),
            Rect { height: 1, ..inner },
        );
        let entries = self.entries();
        let rows: Vec<_> = if entries.is_empty() {
            vec![ListItem::new("No matching settings")]
        } else {
            entries
                .iter()
                .map(|s| {
                    ListItem::new(vec![
                        Line::from(format!("{}: {}", s.title(), s.value(values))),
                        Line::from(s.description()).style(p.normal().fg(p.muted)),
                    ])
                })
                .collect()
        };
        let mut state =
            ListState::default().with_selected((!entries.is_empty()).then_some(self.cursor));
        frame.render_stateful_widget(
            List::new(rows)
                .highlight_symbol("› ")
                .highlight_style(p.normal().fg(p.accent)),
            Rect {
                y: inner.y + 1,
                height: inner.height - 2,
                ..inner
            },
            &mut state,
        );
        let (footer, color) = match &self.notice {
            Some(Notice::Saved(title)) => (format!("{title} saved · Esc close"), p.good),
            Some(Notice::Failed(reason)) => (reason.clone(), p.bad),
            None => (
                "Changes save immediately · ↑↓ select · Ctrl+U clear".into(),
                p.muted,
            ),
        };
        frame.render_widget(
            Paragraph::new(footer).style(p.normal().fg(color)),
            Rect {
                y: inner.bottom() - 1,
                height: 1,
                ..inner
            },
        );
    }
}
pub fn key(key: KeyEvent, ui: &mut Ui) {
    let Some(mut picker) = ui.settings.take() else {
        return;
    };
    match key.code {
        KeyCode::Esc => return,
        KeyCode::Up => picker.cursor = picker.cursor.saturating_sub(1),
        KeyCode::Down => {
            picker.cursor = (picker.cursor + 1).min(picker.entries().len().saturating_sub(1))
        }
        KeyCode::Home => picker.cursor = 0,
        KeyCode::End => picker.cursor = picker.entries().len().saturating_sub(1),
        KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
            picker.query.clear();
            picker.cursor = 0;
        }
        KeyCode::Enter | KeyCode::Left | KeyCode::Right if key.modifiers.is_empty() => {
            if let Some(setting) = picker.entries().get(picker.cursor) {
                let mut values = ui.preferences.values;
                if matches!(setting, Setting::Theme) {
                    values.light = ui.palette.light;
                }
                setting.change(&mut values, key.code == KeyCode::Left);
                match ui.preferences.save(values) {
                    Ok(()) => {
                        if matches!(setting, Setting::Theme) {
                            ui.palette = Palette::new(values.light);
                        }
                        picker.notice = Some(Notice::Saved(setting.title()));
                    }
                    Err(error) => {
                        picker.notice = Some(Notice::Failed(
                            grapt_tui_transcript::markdown::literal(&error),
                        ))
                    }
                }
            }
        }
        KeyCode::Backspace => {
            picker.query.pop();
            picker.cursor = 0;
        }
        KeyCode::Char(ch)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            picker.paste(&ch.to_string())
        }
        _ => {}
    }
    ui.settings = Some(picker);
}
#[cfg(test)]
mod tests;
