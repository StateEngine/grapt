//! Bounded read-only command results; no editor or protocol ownership.
use crossterm::event::KeyCode;
use grapt_tui_transcript::palette::Palette;
use ratatui::{
    layout::Rect,
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

const MAX_BYTES: usize = 262_144;
const MAX_ROWS: usize = 8_192;
const MAX_WRAPPED: usize = 65_536;
const TRUNCATED: &str = "[Result truncated in this view]";

pub struct Pane {
    rows: Vec<String>,
    wrapped: Vec<String>,
    width: usize,
    scroll: usize,
    page: usize,
}
impl Pane {
    pub fn new(rows: Vec<String>) -> Self {
        let mut kept = Vec::new();
        let mut bytes = 0;
        let mut rows = rows.into_iter().peekable();
        while let Some(row) = rows.next() {
            let mut row = grapt_tui_transcript::markdown::literal(&row);
            let remaining = MAX_BYTES.saturating_sub(bytes);
            let truncated = row.len() > remaining;
            if truncated {
                let mut end = remaining;
                while !row.is_char_boundary(end) {
                    end = end.saturating_sub(1);
                }
                row.truncate(end);
            }
            bytes += row.len();
            kept.push(row);
            if truncated
                || ((kept.len() >= MAX_ROWS || bytes >= MAX_BYTES) && rows.peek().is_some())
            {
                kept.push(TRUNCATED.into());
                break;
            }
        }
        Self {
            rows: kept,
            wrapped: Vec::new(),
            width: 0,
            scroll: 0,
            page: 1,
        }
    }
    pub fn replace(&mut self, rows: Vec<String>) -> bool {
        let mut next = Self::new(rows);
        if next.rows == self.rows {
            return false;
        }
        next.scroll = self.scroll;
        *self = next;
        true
    }
    pub fn key(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Esc => return true,
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(self.page),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(self.page),
            KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
            KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = usize::MAX,
            _ => {}
        }
        false
    }
    pub fn draw(&mut self, frame: &mut Frame<'_>, mut area: Rect, p: Palette) {
        if area.width < 30 || area.height < 7 {
            area = frame.area();
        }
        frame.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Command result ")
            .style(p.normal());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let content = Rect {
            height: inner.height.saturating_sub(2),
            ..inner
        };
        let width = usize::from(content.width.max(1));
        if self.width != width {
            self.width = width;
            self.wrapped.clear();
            for row in &self.rows {
                // Each logical row is already bounded. Bound physical rows too
                // so a one-column terminal cannot multiply retained output.
                let wrapped = grapt_client::cells::wrap(row, width);
                self.wrapped.extend(
                    wrapped
                        .into_iter()
                        .take(MAX_WRAPPED.saturating_sub(self.wrapped.len())),
                );
                if self.wrapped.len() >= MAX_WRAPPED {
                    self.wrapped.push(TRUNCATED.into());
                    break;
                }
            }
        }
        self.page = usize::from(content.height).saturating_sub(1).max(1);
        self.scroll = self.scroll.min(
            self.wrapped
                .len()
                .saturating_sub(usize::from(content.height)),
        );
        let end = self
            .scroll
            .saturating_add(usize::from(content.height))
            .min(self.wrapped.len());
        frame.render_widget(
            Paragraph::new(self.wrapped[self.scroll..end].join("\n")),
            content,
        );
        frame.render_widget(
            Paragraph::new(format!(
                "Rows {}-{}/{} · PgUp/PgDn\nHome/End · Esc close",
                self.scroll + usize::from(end > self.scroll),
                end,
                self.wrapped.len()
            ))
            .style(p.normal().fg(p.accent)),
            Rect {
                y: content.bottom(),
                height: inner.height.min(2),
                ..inner
            },
        );
    }
}

#[cfg(test)]
mod tests;
