// Copyright 2023-2026 SpaceXAI. Licensed under Apache-2.0.
// at 2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8. Uses Grapt's existing window.
//! Geometry only: cached row counts, with small estimates for unmeasured items.
use super::Transcript;
use crate::palette::Palette;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    widgets::{Block, Clear, Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget},
    Frame,
};

mod drag;
pub(super) use drag::Drag;

#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub area: Rect,
    pub thumb: Rect,
    pub track: Rect,
    pub visible: usize,
    pub maximum: usize,
}

impl Transcript {
    /// Hit testing uses the very same widget and cached metrics as painting.
    /// Textarea scrollbar hit test; no unseen layout.
    pub fn scrollbar_geometry(&self, area: Rect) -> Option<Geometry> {
        if area.is_empty() {
            return None;
        }
        let (length, position, visible) = self.scrollbar_metrics()?;
        let area = Rect::new(area.right().saturating_sub(1), area.y, 1, area.height);
        let maximum = length.saturating_sub(visible);
        let mut state = ScrollbarState::new(maximum.saturating_add(1))
            .position(position)
            .viewport_content_length(visible);
        let mut buffer = Buffer::empty(area);
        widget(area).render(area, &mut buffer, &mut state);
        let first = (area.y..area.bottom()).find(|y| buffer[(area.x, *y)].symbol() == "█")?;
        let count = (first..area.bottom())
            .take_while(|y| buffer[(area.x, *y)].symbol() == "█")
            .count() as u16;
        Some(Geometry {
            area,
            thumb: Rect::new(area.x, first, 1, count),
            track: if area.height >= 3 {
                Rect::new(area.x, area.y + 1, 1, area.height - 2)
            } else {
                area
            },
            visible,
            maximum,
        })
    }

    pub(crate) fn seek(&mut self, position: usize, maximum: usize) {
        self.seek = None;
        self.tail_anchor = None;
        self.overflow = None;
        self.from_top = position < maximum;
        self.scroll = if self.from_top { position } else { 0 };
    }

    pub fn draw_scrollbar(&self, frame: &mut Frame<'_>, area: Rect, palette: Palette) {
        frame.render_widget(Clear, area);
        frame.render_widget(Block::default().style(palette.normal()), area);
        let Some((length, position, visible)) = self.scrollbar_metrics() else {
            return;
        };
        let mut state = ScrollbarState::new(length.saturating_sub(visible).saturating_add(1))
            .position(position)
            .viewport_content_length(visible);
        frame.render_stateful_widget(
            widget(area)
                .begin_style(palette.normal().fg(palette.muted))
                .end_style(palette.normal().fg(palette.muted))
                .track_style(palette.normal().fg(palette.muted))
                .thumb_style(palette.normal().fg(if self.away_from_tail() {
                    palette.accent
                } else {
                    palette.muted
                })),
            area,
            &mut state,
        );
    }

    fn scrollbar_metrics(&self) -> Option<(usize, usize, usize)> {
        if let Some(drag) = &self.scrollbar_drag {
            return Some(drag.metrics());
        }
        let (first, last) = if self.from_top {
            (self.window.first()?, self.window.last()?)
        } else {
            (self.window.last()?, self.window.first()?)
        };
        let width = self.view.as_ref()?.width;
        let (mut length, mut start, mut end) = (0usize, 0usize, 0usize);
        for index in 0..self.cells.len() {
            if index == first.index {
                start = length.saturating_add(first.start);
            }
            if index == last.index {
                end = length.saturating_add(last.end);
            }
            let rows = self.estimated_rows(index, width);
            length = length.saturating_add(rows);
        }
        (start > 0 || end < length).then_some((length, start, end.saturating_sub(start).max(1)))
    }
}

fn widget(area: Rect) -> Scrollbar<'static> {
    Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol((area.height >= 3).then_some("▲"))
        .end_symbol((area.height >= 3).then_some("▼"))
        .track_symbol(Some("│"))
        .thumb_symbol("█")
}

#[cfg(test)]
mod tests;
