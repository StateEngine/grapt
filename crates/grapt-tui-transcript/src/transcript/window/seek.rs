//! A scrollbar seeks by cached row weights, then walks only its visible neighborhood.
// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// ::scroll at bb64b894fd67158be0f874872e6d7bfe717e7368. Grapt keeps a
// local entry/row anchor through relative input instead of measuring its prefix.
use super::super::*;

#[derive(Clone, Copy)]
pub(in crate::transcript) struct Seek {
    pub index: usize,
    pub row: usize,
    pub position: usize,
}

impl Transcript {
    pub(in crate::transcript) fn seek_window(
        &mut self,
        width: u16,
        height: u16,
        palette: Palette,
    ) -> bool {
        let Some(seek) = self
            .seek
            .filter(|s| self.from_top && s.position == self.scroll)
        else {
            self.seek = None;
            return false;
        };
        let mut remaining = usize::from(height.max(1));
        for index in seek.index..self.cells.len() {
            if remaining == 0 {
                break;
            }
            let length = self.layout(index, width, palette).len();
            let start = if index == seek.index {
                seek.row.min(length.saturating_sub(1))
            } else {
                0
            };
            let end = length.min(start.saturating_add(remaining));
            if end > start {
                self.window.push(Window { index, start, end });
                remaining -= end - start;
            }
        }
        let at_tail = self.window.last().is_some_and(|last| {
            last.index + 1 == self.cells.len()
                && self.cells[last.index].cache.as_ref().is_some_and(|cache| {
                    last.end == cache.rows(self.cells[last.index].slice.kind).len()
                })
        });
        if at_tail && !self.dragging_scrollbar() {
            self.seek = None;
            self.scroll = 0;
            self.from_top = false;
            self.window(width, height, palette);
            if remaining > 0 {
                self.overflow = Some((false, remaining));
            }
            return true;
        }
        // Fill the viewport at the loaded tail without measuring the old prefix.
        let mut before = self.window.first().copied();
        while remaining > 0 {
            let Some(first) = before else { break };
            let (index, end) = if first.start > 0 {
                (first.index, first.start)
            } else if first.index > 0 {
                let index = first.index - 1;
                (index, self.layout(index, width, palette).len())
            } else {
                break;
            };
            let start = end.saturating_sub(remaining);
            if end > start {
                self.window.insert(0, Window { index, start, end });
                remaining -= end - start;
            }
            before = Some(Window { index, start, end });
        }
        if let Some(first) = self.window.first() {
            self.seek = Some(Seek {
                index: first.index,
                row: first.start,
                ..seek
            });
        }
        true
    }

    /// Relative input after a grab keeps the same reading anchor, without a prefix scan.
    pub fn scroll_seek(&mut self, older: bool, mut amount: usize) -> Option<usize> {
        let mut seek = self.seek?;
        if !self.from_top || seek.position != self.scroll {
            self.seek = None;
            return None;
        }
        let view = self.view.as_ref()?;
        let (width, palette) = (view.width, Palette::new(view.light));
        loop {
            if older {
                if amount <= seek.row {
                    seek.row -= amount;
                    amount = 0;
                    break;
                }
                amount -= seek.row;
                if seek.index == 0 {
                    seek.row = 0;
                    break;
                }
                seek.index -= 1;
                seek.row = self.layout(seek.index, width, palette).len();
            } else {
                let length = self.layout(seek.index, width, palette).len();
                let available = length.saturating_sub(seek.row);
                if amount < available {
                    seek.row += amount;
                    amount = 0;
                    break;
                }
                amount -= available;
                if seek.index + 1 == self.cells.len() {
                    self.seek = None;
                    self.from_top = false;
                    self.scroll = 0;
                    return Some(amount);
                }
                seek.index += 1;
                seek.row = 0;
            }
        }
        self.scroll = self
            .estimated_prefix(seek.index, width)
            .saturating_add(seek.row);
        seek.position = self.scroll;
        self.seek = Some(seek);
        self.revision = self.revision.saturating_add(1);
        Some(amount)
    }
}
