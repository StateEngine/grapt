//! One grab owns a fixed range and one bounded pending history replacement.
use super::super::*;

pub(in crate::transcript) struct Drag {
    area: Rect,
    maximum: usize,
    visible: usize,
    position: usize,
    ends: Vec<usize>,
    replacement: Option<Vec<Slice>>,
}

impl Drag {
    pub(super) fn metrics(&self) -> (usize, usize, usize) {
        (
            self.maximum.saturating_add(self.visible),
            self.position,
            self.visible,
        )
    }
}

impl Transcript {
    pub(in crate::transcript) fn estimated_rows(&self, index: usize, width: u16) -> usize {
        let cell = &self.cells[index];
        if self.hidden_tool(index) || cell.slice.kind == SliceKind::Reasoning {
            return 0;
        }
        cell.height.filter(|(w, _)| *w == width).map_or_else(
            || {
                if cell.slice.tool_phase == Some(ToolPhase::Streaming) {
                    0
                } else {
                    3
                }
            },
            |(_, rows)| rows,
        )
    }

    pub(in crate::transcript) fn estimated_prefix(&self, index: usize, width: u16) -> usize {
        (0..index)
            .map(|i| self.estimated_rows(i, width))
            .fold(0, usize::saturating_add)
    }

    pub fn dragging_scrollbar(&self) -> bool {
        self.scrollbar_drag.is_some()
    }

    pub fn begin_scrollbar_drag(&mut self, area: Rect) -> bool {
        let Some(bar) = self.scrollbar_geometry(area) else {
            return false;
        };
        let Some(view) = &self.view else { return false };
        let Some((_, position, _)) = self.scrollbar_metrics() else {
            return false;
        };
        let mut length = 0usize;
        // Count follows the existing bounded projection; only cached metadata is read.
        let ends = (0..self.cells.len())
            .map(|index| {
                length = length.saturating_add(self.estimated_rows(index, view.width));
                length
            })
            .collect();
        self.scrollbar_drag = Some(Drag {
            area: bar.area,
            maximum: bar.maximum,
            visible: bar.visible,
            position,
            ends,
            replacement: None,
        });
        true
    }

    pub fn drag_scrollbar(&mut self, area: Rect, row: u16, offset: u16) {
        let Some(bar) = self.scrollbar_geometry(area) else {
            return;
        };
        let Some(drag) = &self.scrollbar_drag else {
            return;
        };
        if drag.area != area {
            return;
        }
        let travel = usize::from(bar.track.height.saturating_sub(bar.thumb.height));
        if travel == 0 {
            return;
        }
        let row = usize::from(
            row.saturating_sub(bar.track.y)
                .saturating_sub(offset.min(bar.thumb.height.saturating_sub(1))),
        )
        .min(travel);
        let position = row.saturating_mul(drag.maximum).saturating_add(travel / 2) / travel;
        let index = drag
            .ends
            .partition_point(|end| *end <= position)
            .min(drag.ends.len().saturating_sub(1));
        let start = index.checked_sub(1).map_or(0, |i| drag.ends[i]);
        let weight = drag.ends[index].saturating_sub(start).max(1);
        let maximum = drag.maximum;
        self.seek(position, maximum);
        if position < maximum {
            let Some(view) = &self.view else { return };
            let (width, palette) = (view.width, Palette::new(view.light));
            let length = self.layout(index, width, palette).len();
            let row = position.saturating_sub(start).saturating_mul(length) / weight;
            self.seek = Some(super::super::window::Seek {
                index,
                row: row.min(length.saturating_sub(1)),
                position,
            });
        }
        if let Some(drag) = &mut self.scrollbar_drag {
            drag.position = position;
        }
        self.overflow = None;
    }

    pub fn end_scrollbar_drag(&mut self) {
        if let Some(rows) = self.scrollbar_drag.take().and_then(|d| d.replacement) {
            self.reconcile(rows);
        }
    }

    pub(in crate::transcript) fn defer_drag_update(
        &mut self,
        reset: bool,
        start: usize,
        rows: Vec<Slice>,
    ) -> Option<Vec<Slice>> {
        let Some(drag) = &mut self.scrollbar_drag else {
            return Some(rows);
        };
        if reset {
            // Replace the prior full projection rather than queueing historical copies.
            drag.replacement = Some(rows);
            return None;
        }
        let Some(pending) = &mut drag.replacement else {
            return Some(rows);
        };
        for (offset, row) in rows.into_iter().enumerate() {
            let index = start.saturating_add(offset);
            if let Some(previous) = pending.get_mut(index) {
                *previous = row;
            } else if index == pending.len() {
                pending.push(row);
            }
        }
        None
    }
}
