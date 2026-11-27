//! Keep the visible entry while new output or durable reconciliation changes history.
// Copyright 2025 OpenAI. Licensed under Apache-2.0.
use super::Transcript;
use grapt_client::Slice;
use std::collections::{BTreeMap, BTreeSet};

// Resolve the old visible entry
// after a history rebuild, rather than interpreting it as return-to-latest.
struct Reading {
    source: Slice,
    index: usize,
    row: usize,
    width: u16,
    from_top: bool,
    expanded: bool,
    seek: bool,
}

#[derive(Clone, Copy)]
pub(super) struct Tail {
    pub index: usize,
    pub rows: usize,
    pub width: u16,
}

impl Transcript {
    pub fn away_from_tail(&self) -> bool {
        self.from_top || self.scroll > 0
    }

    /// Durable reconciliation rebuilds this same page; selection and paging
    /// continue to use update(reset), which intentionally starts a new view.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn reconcile(&mut self, rows: Vec<Slice>) -> bool {
        let Some(rows) = self.defer_drag_update(true, 0, rows) else {
            return true;
        };
        let reading = self.reading_anchor();
        let expanded: BTreeSet<_> = self
            .cells
            .iter()
            .filter(|cell| cell.expanded)
            .filter_map(|cell| cell.slice.tool_id.clone())
            .collect();
        let diffs: BTreeMap<_, _> = self
            .cells
            .iter()
            .filter_map(|cell| Some((cell.slice.tool_id.clone()?, cell.diff.as_ref()?.expanded())))
            .collect();
        let compactions: BTreeMap<_, _> = self
            .cells
            .iter()
            .filter_map(|cell| {
                Some((
                    cell.slice.compaction.as_ref()?.id.clone(),
                    (cell.expanded, cell.observed_at),
                ))
            })
            .collect();
        let groups = self.tool_groups.expanded_ids();
        let changed = self.update(true, 0, rows);
        self.tool_groups.restore_expanded(&groups);
        for cell in &mut self.cells {
            if let Some((expanded, observed)) = cell
                .slice
                .compaction
                .as_ref()
                .and_then(|info| compactions.get(&info.id))
            {
                cell.expanded = *expanded;
                cell.observed_at = *observed;
            }
            if let Some(diff) = &mut cell.diff {
                if let Some(files) = cell.slice.tool_id.as_ref().and_then(|id| diffs.get(id)) {
                    diff.restore(files);
                }
            }
            if cell
                .slice
                .tool_id
                .as_ref()
                .is_some_and(|id| expanded.contains(id))
            {
                cell.expanded = true;
            }
        }
        if let Some(reading) = reading.filter(|_| !self.cells.is_empty()) {
            let matches = |index: usize| {
                let slice = &self.cells[index].slice;
                if let Some(footer) = &reading.source.footer {
                    slice
                        .footer
                        .as_ref()
                        .is_some_and(|candidate| candidate.message == footer.message)
                } else if let Some(info) = &reading.source.compaction {
                    slice
                        .compaction
                        .as_ref()
                        .is_some_and(|candidate| candidate.id == info.id)
                } else if reading.source.tool_id.is_some() {
                    slice.tool_id == reading.source.tool_id
                } else {
                    slice.kind == reading.source.kind && slice.text == reading.source.text
                }
            };
            let index = if reading.index < self.cells.len() && matches(reading.index) {
                reading.index
            } else {
                (0..self.cells.len())
                    .find(|index| matches(*index))
                    .unwrap_or_else(|| reading.index.min(self.cells.len() - 1))
            };
            self.cells[index].expanded = reading.expanded;
            self.from_top = reading.from_top;
            if reading.seek {
                self.scroll = self
                    .estimated_prefix(index, reading.width)
                    .saturating_add(reading.row);
                self.from_top = true;
                self.seek = Some(super::window::Seek {
                    index,
                    row: reading.row,
                    position: self.scroll,
                });
                return changed;
            }
            let height = self.height(index, reading.width);
            self.scroll = if reading.from_top {
                (0..index)
                    .map(|i| self.height(i, reading.width))
                    .sum::<usize>()
                    .saturating_add(reading.row.min(height.saturating_sub(1)))
            } else {
                (index + 1..self.cells.len())
                    .map(|i| self.height(i, reading.width))
                    .sum::<usize>()
                    .saturating_add(height.saturating_sub(reading.row))
            };
        }
        changed
    }

    fn reading_anchor(&self) -> Option<Reading> {
        if !self.away_from_tail() {
            return None;
        }
        let edge = self.window.first()?;
        Some(Reading {
            source: self.cells.get(edge.index)?.slice.clone(),
            index: edge.index,
            row: if self.from_top { edge.start } else { edge.end },
            width: self.view.as_ref()?.width,
            from_top: self.from_top,
            expanded: self.cells.get(edge.index)?.expanded,
            seek: self.seek.is_some(),
        })
    }

    pub(super) fn pin_tail(&mut self) {
        if self.tail_anchor.is_some() || self.from_top || self.scroll == 0 {
            return;
        }
        let Some(view) = &self.view else { return };
        let Some(cell) = self.cells.last() else {
            return;
        };
        let rows = match cell.height {
            Some((width, rows)) if width == view.width => rows,
            _ if cell.slice.tool_phase == Some(grapt_client::projection::ToolPhase::Streaming) => 0,
            _ => return,
        };
        self.tail_anchor = Some(Tail {
            index: self.cells.len() - 1,
            rows,
            width: view.width,
        });
    }
}

#[cfg(test)]
mod tests;
