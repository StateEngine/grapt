use super::*;
use grapt_client::cells::{cells, tail};
use grapt_tui_transcript::{markdown::literal, palette::Palette};
use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

// Gregorian dates from provider-reported milliseconds only; no wall clock.
fn date(stamp: Option<u64>) -> String {
    let Some(stamp) = stamp else {
        return "Date unavailable".into();
    };
    let days = stamp / 86_400_000;
    let mut year = 1970 + days / 146_097 * 400;
    let mut day = days % 146_097;
    let leap = |year: u64| {
        year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
    };
    for _ in 0..400 {
        let length = if leap(year) { 366 } else { 365 };
        if day < length {
            break;
        }
        day -= length;
        year += 1;
    }
    let months = [
        31,
        if leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1;
    for length in months {
        if day < length {
            break;
        }
        day -= length;
        month += 1;
    }
    format!("{year:04}-{month:02}-{:02} UTC", day + 1)
}
fn short_path(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if cells(text) <= width {
        text.to_owned()
    } else {
        format!("…{}", tail(text, width.saturating_sub(1)))
    }
}
impl Picker {
    fn category(&self, row: &Agent, pins: &Pins) -> String {
        if pins.contains(&row.row.id) {
            return "Pinned".into();
        }
        match self.group {
            Group::Date => date(row.row.detail.updated_ms),
            Group::Directory => row
                .row
                .detail
                .directory
                .as_deref()
                .map(literal)
                .unwrap_or_else(|| "Directory unavailable".into()),
        }
    }
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn draw(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        agents: &[Agent],
        pins: &Pins,
        catalog: CatalogReach,
        p: Palette,
    ) {
        frame.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Sessions · Enter open · Esc close ")
            .title_bottom(
                " Ctrl+B pin · Ctrl+R rename · Ctrl+D delete · Ctrl+G group · Ctrl+U clear ",
            )
            .style(p.normal().bg(p.panel));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height == 0 {
            return;
        }
        frame.render_widget(
            Paragraph::new(format!("Search: {}", literal(&self.query)))
                .style(p.normal().fg(p.muted)),
            Rect { height: 1, ..inner },
        );
        let list = Rect {
            y: inner.y.saturating_add(1),
            height: inner.height.saturating_sub(3),
            ..inner
        };
        self.page = usize::from(list.height).max(1);
        let entries = self.entries(agents, pins);
        let mut rows = Vec::new();
        let mut category = String::new();
        let mut selected = None;
        for row in &entries {
            let next = self.category(row, pins);
            if next != category {
                let label = if self.group == Group::Directory {
                    short_path(&next, usize::from(list.width).saturating_sub(4))
                } else {
                    next.clone()
                };
                rows.push(ListItem::new(Line::from(Span::styled(
                    label,
                    p.normal().fg(p.muted),
                ))));
                category = next;
            }
            if Some(&row.row.id) == self.selected.as_ref() {
                selected = Some(rows.len());
            }
            let slot = pins
                .position(&row.row.id)
                .filter(|n| *n < 9)
                .map_or_else(|| "  ".into(), |n| format!("{} ", n + 1));
            let title = literal(&row.row.title).replace('\n', "\\n");
            let title = match row.row.detail.metadata_status {
                Some(grapt_api::u3a::SessionInfoDtoMetadataStatus::Unavailable) => {
                    format!("{title} (unavailable)")
                }
                Some(grapt_api::u3a::SessionInfoDtoMetadataStatus::Quarantined) => {
                    format!("{title} (quarantined)")
                }
                None => title,
            };
            let directory = row
                .row
                .detail
                .directory
                .as_deref()
                .map(literal)
                .unwrap_or_else(|| "Directory unavailable".into());
            let live = agents.iter().find(|a| a.row.id == row.row.id);
            let status = literal(live.map_or(row.status.as_str(), |a| a.status.as_str()));
            let path_width =
                usize::from(list.width).saturating_sub(cells(&title) + cells(&status) + 11);
            let directory = short_path(&directory, path_width);
            rows.push(ListItem::new(Line::from(vec![
                Span::styled(slot, p.normal().fg(p.accent)),
                Span::styled(
                    if live.is_some_and(|a| a.current) {
                        "● "
                    } else {
                        "  "
                    },
                    p.normal().fg(p.good),
                ),
                Span::raw(if row.row.parent.is_some() { "↳ " } else { "" }),
                Span::raw(title),
                Span::styled(format!("  {status} · {directory}"), p.normal().fg(p.muted)),
            ])));
        }
        let count = entries.len();
        drop(entries);
        if count == 0 {
            rows.push(ListItem::new(self.empty_note(catalog)));
        }
        let mut state = ListState::default()
            .with_offset(self.offset)
            .with_selected(selected);
        frame.render_stateful_widget(
            List::new(rows)
                .style(p.normal().bg(p.panel))
                .highlight_symbol("› ")
                .highlight_style(p.normal().fg(p.accent).bg(p.panel)),
            list,
            &mut state,
        );
        self.offset = state.offset();
        frame.render_widget(
            Paragraph::new(self.search_hint(catalog)).style(p.normal().fg(p.muted)),
            Rect {
                y: inner.bottom().saturating_sub(2),
                height: 1,
                ..inner
            },
        );
        frame.render_widget(
            Paragraph::new(format!("{} matches · ↑↓ PgUp/PgDn · Esc close", count))
                .style(p.normal().fg(p.muted)),
            Rect {
                y: inner.bottom().saturating_sub(1),
                height: 1,
                ..inner
            },
        );
        if let Some(edit) = &mut self.edit {
            let popup = Rect {
                x: inner.x,
                y: inner.y.saturating_add(inner.height / 2).saturating_sub(2),
                width: inner.width,
                height: inner.height.min(4),
            };
            frame.render_widget(Clear, popup);
            let (title, style) = match edit {
                Edit::Rename { .. } => (
                    " Rename session · Enter save · Esc cancel ",
                    p.normal().fg(p.accent),
                ),
                Edit::Delete { .. } => (
                    " Delete session and its history? · Enter delete · Esc cancel ",
                    p.normal().fg(p.bad),
                ),
            };
            let block = Block::default()
                .borders(Borders::ALL)
                .title(if self.pending.is_some() {
                    " Saving… · awaiting engine confirmation "
                } else {
                    title
                })
                .style(style.bg(p.panel));
            let content = block.inner(popup);
            frame.render_widget(block, popup);
            match edit {
                Edit::Rename { input, .. } => frame.render_widget(&**input, content),
                Edit::Delete { id } => {
                    let label = agents
                        .iter()
                        .find(|a| &a.row.id == id)
                        .map(|a| a.row.title.as_str())
                        .unwrap_or_else(|| grapt_client::wire::Spelled::spelled(id));
                    frame.render_widget(
                        Paragraph::new(literal(label)).style(p.normal().fg(p.bad)),
                        content,
                    );
                }
            }
        }
    }
}
