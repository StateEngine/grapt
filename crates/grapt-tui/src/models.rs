//! Catalog names and choices.
//! Copyright 2025 OpenAI (licenses/openai-codex/).
use crossterm::event::KeyCode;
use grapt_client::{
    cells,
    wire::{ModelRow, Spelled},
};
use grapt_tui_transcript::{markdown::literal, palette::Palette};
use ratatui::{
    layout::Rect,
    text::Line,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

fn name(model: &ModelRow) -> String {
    literal(if model.name.is_empty() {
        model.id.spelled()
    } else {
        &model.name
    })
    .replace('\n', " ")
}

pub(crate) fn effort(model: &ModelRow) -> String {
    model
        .variant
        .as_ref()
        .map(|v| literal(v.spelled()).replace('\n', " "))
        .unwrap_or_else(|| {
            model
                .default_effort
                .as_ref()
                .map(|effort| format!("{} (default)", literal(effort.spelled()).replace('\n', " ")))
                .unwrap_or_else(|| "default effort".into())
        })
}

/// The reported tool surface. An unreported capability renders as unknown,
/// never as a denial the catalog did not make.
pub(crate) fn tools(model: &ModelRow) -> &'static str {
    match model.capabilities.tools {
        Some(true) => "tools",
        Some(false) => "no tools",
        None => "tools —",
    }
}

pub fn chip(model: Option<&ModelRow>, width: usize) -> String {
    let Some(model) = model else {
        return cells::cut("Model unavailable", width);
    };
    let suffix = model
        .variant
        .as_ref()
        .or(model.default_effort.as_ref())
        .map(|effort| format!(" · {}", literal(effort.spelled()).replace('\n', " ")))
        .unwrap_or_default();
    let prefix = literal(model.id.spelled()).replace('\n', " ");
    let available = width.saturating_sub(cells::cells(&suffix));
    format!(
        "{}{}",
        cells::cut(&prefix, available),
        cells::cut(&suffix, width)
    )
}

fn same(a: &ModelRow, b: &ModelRow) -> bool {
    a.id == b.id && a.provider_id == b.provider_id && a.variant == b.variant
}

#[derive(Default)]
pub struct Picker {
    rows: Option<Vec<ModelRow>>,
    selected: usize,
    query: String,
    offset: usize,
    page: usize,
    pub pending: bool,
}

impl Picker {
    pub fn loaded(&mut self, rows: Vec<ModelRow>, current: Option<&ModelRow>) {
        self.selected = current
            .and_then(|current| rows.iter().position(|row| same(row, current)))
            .unwrap_or(0);
        self.rows = Some(rows);
        self.filtered();
    }

    fn entries(&self) -> Vec<usize> {
        let query = self.query.to_lowercase();
        self.rows
            .as_deref()
            .unwrap_or_default()
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let label = format!(
                    "{} {} {} {}",
                    name(row),
                    row.provider_id.spelled(),
                    effort(row),
                    tools(row)
                );
                label.to_lowercase().contains(&query).then_some(index)
            })
            .collect()
    }

    pub fn selected(&self) -> Option<&ModelRow> {
        let rows = self.rows.as_ref()?;
        self.entries()
            .contains(&self.selected)
            .then(|| rows.get(self.selected))
            .flatten()
    }

    pub fn insert(&mut self, text: &str) -> bool {
        if self.query.len().saturating_add(text.len()) > 256 {
            return false;
        }
        self.query
            .extend(text.chars().filter(|ch| !ch.is_control()));
        self.filtered();
        true
    }

    pub fn clear_filter(&mut self) {
        self.query.clear();
        self.filtered();
    }

    fn filtered(&mut self) {
        let entries = self.entries();
        if !entries.contains(&self.selected) {
            self.selected = entries.first().copied().unwrap_or(0);
        }
        self.offset = 0;
    }

    pub fn key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char(ch) if !ch.is_control() => {
                self.insert(&ch.to_string());
                return;
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.filtered();
                return;
            }
            _ => {}
        }
        let entries = self.entries();
        let current = entries
            .iter()
            .position(|&index| index == self.selected)
            .unwrap_or(0);
        let last = entries.len().saturating_sub(1);
        let next = match key {
            KeyCode::Up => current.saturating_sub(1),
            KeyCode::Down => current.saturating_add(1).min(last),
            KeyCode::PageUp => current.saturating_sub(self.page.max(1)),
            KeyCode::PageDown => current.saturating_add(self.page.max(1)).min(last),
            KeyCode::Home => 0,
            KeyCode::End => last,
            _ => return,
        };
        self.selected = entries.get(next).copied().unwrap_or(0);
    }

    pub fn draw(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        current: Option<&ModelRow>,
        p: Palette,
    ) {
        frame.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(if self.pending {
                " Selecting model… "
            } else {
                " Models and effort · Enter select · Esc close "
            })
            .title_bottom(" Ctrl+U clear filter ")
            .style(p.normal().bg(p.panel));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(format!("Search: {}", literal(&self.query)))
                .style(p.normal().fg(p.muted)),
            Rect {
                height: inner.height.min(1),
                ..inner
            },
        );
        let list = Rect {
            y: inner.y.saturating_add(u16::from(inner.height > 0)),
            height: inner.height.saturating_sub(2),
            ..inner
        };
        self.page = usize::from(list.height).max(1);
        let entries = self.entries();
        let Some(rows) = self.rows.as_ref() else {
            frame.render_widget(Paragraph::new("Loading offered models…"), list);
            return;
        };
        if entries.is_empty() {
            frame.render_widget(
                Paragraph::new(if rows.is_empty() {
                    "No models offered"
                } else {
                    "No matching models"
                }),
                list,
            );
        } else {
            let items = entries
                .iter()
                .map(|&index| {
                    let row = &rows[index];
                    ListItem::new(Line::from(format!(
                        "{}{} · {} · {} · {}{}",
                        if current.is_some_and(|current| same(row, current)) {
                            "● "
                        } else {
                            "  "
                        },
                        name(row),
                        literal(row.provider_id.spelled()),
                        effort(row),
                        tools(row),
                        if row.enabled { "" } else { " · disabled" }
                    )))
                })
                .collect::<Vec<_>>();
            let mut state = ListState::default()
                .with_offset(self.offset)
                .with_selected(entries.iter().position(|&index| index == self.selected));
            frame.render_stateful_widget(
                List::new(items)
                    .style(p.normal().bg(p.panel))
                    .highlight_symbol("› ")
                    .highlight_style(p.normal().fg(p.accent).bg(p.panel)),
                list,
                &mut state,
            );
            self.offset = state.offset();
        }
        if inner.height > 1 {
            frame.render_widget(
                Paragraph::new("↑↓ PgUp/PgDn · type to filter · Enter select · Esc close")
                    .style(p.normal().fg(p.muted)),
                Rect {
                    y: inner.bottom().saturating_sub(1),
                    height: 1,
                    ..inner
                },
            );
        }
    }
}

#[cfg(test)]
mod tests;
