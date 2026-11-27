//! Viewport row identities borrow pixels from the presenter's prior canvas.
//! Cache identity and lifetime for the retained canvas.
// Copyright 2023-2026 SpaceXAI. Modified by Grapt.
use super::PaintedRow;
use crate::palette::Palette;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::Line,
    widgets::{Block, Clear, Widget},
};
use std::collections::BTreeMap;

const MAX_CELLS: usize = 32_768;

#[derive(Default)]
struct CachedRow {
    previous: Option<Rect>,
    current: Option<Rect>,
}

#[derive(Default)]
pub(super) struct Raster {
    key: Option<(u16, bool)>,
    limit: usize,
    used: usize,
    rows: BTreeMap<PaintedRow, CachedRow>,
    #[cfg(test)]
    rendered: usize,
}

impl Raster {
    pub fn begin(&mut self, area: Rect, palette: Palette) {
        let key = (area.width, palette.light);
        if self.key != Some(key) {
            self.rows.clear();
            self.key = Some(key);
        }
        self.limit = usize::from(area.height).min(MAX_CELLS / usize::from(area.width.max(1)));
        self.used = 0;
        for row in self.rows.values_mut() {
            row.previous = row.current.take();
        }
    }

    /// `previous` is the complete canvas owned by the caller, before this paint.
    pub fn paint(
        &mut self,
        buffer: &mut Buffer,
        area: Rect,
        key: PaintedRow,
        line: &Line<'_>,
        palette: Palette,
        previous: Option<&Buffer>,
    ) {
        if self.used == self.limit {
            Clear.render(area, buffer);
            Block::default()
                .style(palette.normal())
                .render(area, buffer);
            line.render(area, buffer);
            return;
        }
        self.used += 1;
        let reused = self
            .rows
            .get(&key)
            .and_then(|row| row.previous)
            .zip(previous)
            .is_some_and(|(source, canvas)| copy_row(buffer, area, canvas, source));
        if !reused {
            // A temporary row preserves clipping/widget semantics without
            // retaining another copy of the presenter's visible pixels.
            let local = Rect::new(0, 0, area.width, 1);
            let mut row = Buffer::empty(local);
            row.set_style(local, palette.normal());
            line.render(local, &mut row);
            let _ = copy_row(buffer, area, &row, local);
            #[cfg(test)]
            {
                self.rendered += 1;
            }
        }
        self.rows.entry(key).or_default().current = Some(area);
    }

    pub fn keep(&mut self, key: PaintedRow, area: Rect) {
        if self.used == self.limit {
            return;
        }
        self.used += 1;
        self.rows.entry(key).or_default().current = Some(area);
    }

    pub fn finish(&mut self) {
        self.rows.retain(|_, row| row.current.is_some());
    }
}

fn copy_row(target: &mut Buffer, destination: Rect, source: &Buffer, from: Rect) -> bool {
    let clipped = destination.intersection(target.area);
    if clipped.is_empty() {
        return true;
    }
    if from.width != destination.width || from.height != 1 || destination.height != 1 {
        return false;
    }
    let Some(x) = from.x.checked_add(clipped.x - destination.x) else {
        return false;
    };
    let source_row = Rect::new(x, from.y, clipped.width, 1);
    if source_row.intersection(source.area) != source_row {
        return false;
    }
    let start = usize::from(clipped.y - target.area.y) * usize::from(target.area.width)
        + usize::from(clipped.x - target.area.x);
    let other = usize::from(from.y - source.area.y) * usize::from(source.area.width)
        + usize::from(x - source.area.x);
    let count = usize::from(clipped.width);
    target.content[start..start + count].clone_from_slice(&source.content[other..other + count]);
    true
}

#[cfg(test)]
mod tests;
