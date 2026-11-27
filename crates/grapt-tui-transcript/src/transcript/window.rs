//! Resolve scroll positions against the full viewport at either loaded edge.
// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// Normalize the far edge to the last full viewport instead of
// permitting a partial, emptying window.
use super::*;
mod seek;
pub(super) use seek::Seek;

impl Transcript {
    /// Cache only visible row ranges; painting borrows the retained layouts.
    pub(super) fn window(&mut self, width: u16, height: u16, palette: Palette) {
        let key = ViewKey {
            revision: self.revision,
            width,
            height,
            scroll: self.scroll,
            from_top: self.from_top,
            light: palette.light,
        };
        if self.view.as_ref() != Some(&key) {
            self.renderer.new_view();
            self.view = Some(key);
        }
        self.overflow = None;
        self.window.clear();
        if self.seek_window(width, height, palette) {
            return;
        }
        let mut skip = self.scroll;
        let mut extent = 0usize;
        let mut reached_end = self.cells.is_empty();
        let anchor = self
            .tail_anchor
            .take()
            .filter(|anchor| !self.from_top && self.scroll > 0 && anchor.width == width);
        let mut remaining = usize::from(height.max(1));
        for position in 0..self.cells.len() {
            if remaining == 0 {
                break;
            }
            let index = if self.from_top {
                position
            } else {
                self.cells.len() - position - 1
            };
            let length = if skip == 0 {
                self.layout(index, width, palette).len()
            } else {
                self.height(index, width)
            };
            extent = extent.saturating_add(length);
            reached_end = position + 1 == self.cells.len();
            if let Some(anchor) = anchor.filter(|anchor| index >= anchor.index) {
                let previous = if index == anchor.index {
                    anchor.rows
                } else {
                    0
                };
                self.scroll = self.scroll.saturating_add(length).saturating_sub(previous);
                skip = skip.saturating_add(length).saturating_sub(previous);
            }
            if skip >= length {
                skip -= length;
                continue;
            }
            let (start, end) = if self.from_top {
                (skip, length.min(skip.saturating_add(remaining)))
            } else {
                let end = length - skip;
                (end.saturating_sub(remaining), end)
            };
            remaining -= end - start;
            self.window.push(Window { index, start, end });
            skip = 0;
        }
        if let Some(view) = &mut self.view {
            view.scroll = self.scroll;
        }
        if reached_end && self.scroll != 0 {
            let maximum = extent.saturating_sub(usize::from(height.max(1)));
            if self.scroll >= maximum {
                let older = !self.from_top;
                let crossed = self.scroll > maximum;
                // Normalize to the opposite edge, resolving a
                // reading position at bottom_start to Latest. This pins resize
                // and repeated input without measuring any unseen history.
                let remaining = self.scroll.saturating_sub(extent);
                self.scroll = 0;
                self.from_top = older;
                self.window(width, height, palette);
                if crossed && !self.cells.is_empty() {
                    self.overflow = Some((older, remaining));
                }
            }
        }
    }
}
