//! Read-only usage disclosure.
//! Copyright 2025 OpenAI (licenses/openai-codex/).
//! Modified by Grapt: shared usage rows in a bounded, read-only viewport.
//! The shared status presenter owns words; this pane owns only its viewport.
use crossterm::event::KeyCode;
use grapt_client::view::status::Snapshot;
use grapt_tui_transcript::palette::Palette;
use ratatui::{
    layout::Rect,
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

#[derive(Default)]
pub struct Pane {
    scroll: usize,
    page: usize,
}
impl Pane {
    pub fn key(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Esc | KeyCode::F(4) => return true,
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(self.page.max(1)),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(self.page.max(1)),
            KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
            KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = usize::MAX,
            _ => {}
        }
        false
    }
    pub fn draw(
        &mut self,
        frame: &mut Frame<'_>,
        mut area: Rect,
        usage: &Snapshot,
        policy: Option<&str>,
        p: Palette,
    ) {
        if area.width < 30 || area.height < 7 {
            area = frame.area();
        }
        frame.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Usage · live session ")
            .style(p.normal());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let content = Rect {
            height: inner.height.saturating_sub(2),
            ..inner
        };
        let mut rows = usage.lines(usize::from(content.width.max(1)));
        if let Some(policy) = policy {
            rows.splice(
                1..1,
                grapt_client::cells::wrap(
                    &grapt_tui_transcript::markdown::literal(policy).replace('\n', " "),
                    usize::from(content.width.max(1)),
                ),
            );
        }
        self.scroll = self
            .scroll
            .min(rows.len().saturating_sub(usize::from(content.height)));
        self.page = usize::from(content.height).saturating_sub(1).max(1);
        let end = self
            .scroll
            .saturating_add(usize::from(content.height))
            .min(rows.len());
        frame.render_widget(Paragraph::new(rows[self.scroll..end].join("\n")), content);
        frame.render_widget(
            Paragraph::new(format!(
                "Rows {}-{}/{} · PgUp/PgDn\nHome/End · Esc/F4 close",
                self.scroll + usize::from(end > self.scroll),
                end,
                rows.len()
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
pub(crate) mod tests;
