//! Inline marker.
//! Copyright 2025 OpenAI. Modified by Grapt for its existing palette and cache.
use crate::palette::Palette;
use ratatui::text::{Line, Span};

// Keep one content column in tiny viewports; normal prose has a two-cell gutter.
pub(super) fn width(width: u16) -> u16 {
    width.saturating_sub(width.saturating_sub(1).min(2))
}

pub(super) fn line(
    source: &Line<'static>,
    row: usize,
    first: bool,
    width: u16,
    p: Palette,
) -> Line<'static> {
    let mut line = source.clone();
    let gutter = width.saturating_sub(1).min(2);
    if gutter == 0 || (row > 0 && line.spans.iter().all(|s| s.content.is_empty())) {
        return line;
    }
    let prefix = match (row == 0 && first, gutter) {
        (true, 1) => "●",
        (true, _) => "● ",
        (_, 1) => " ",
        _ => "  ",
    };
    line.spans
        .insert(0, Span::styled(prefix, p.normal().fg(p.muted)));
    line
}

impl super::Transcript {
    /// Multiple text blocks in one reply share a single leading marker.
    pub(super) fn starts_reply(&self, index: usize) -> bool {
        self.cells[..index]
            .iter()
            .rev()
            .find(|cell| {
                !matches!(
                    cell.slice.kind,
                    grapt_client::SliceKind::Reasoning | grapt_client::SliceKind::System
                )
            })
            .is_none_or(|cell| cell.slice.kind != grapt_client::SliceKind::Text)
    }
}
