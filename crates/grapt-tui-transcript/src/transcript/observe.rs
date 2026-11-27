//! Read-only observations of transcript state for the TUI's integration
//! tests. Compiled only for this crate's tests and the default-off
//! `test-support` feature, which the TUI enables only as a dev-dependency;
//! ordinary builds contain none of it.
use super::*;

/// The first visible window row: which cell, and the rows it spans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowRow {
    pub index: usize,
    pub start: usize,
    pub end: usize,
}

/// One painted screen row: the cell it belongs to and its row in that cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaintedCell {
    pub cell: usize,
    pub row: usize,
}

impl Transcript {
    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }

    pub fn cell_expanded(&self, index: usize) -> bool {
        self.cells[index].expanded
    }

    pub fn expanded_cells(&self) -> impl Iterator<Item = bool> + '_ {
        self.cells.iter().map(|cell| cell.expanded)
    }

    pub fn cell_slice(&self, index: usize) -> &Slice {
        &self.cells[index].slice
    }

    /// The cell's inline diff rows as expanded, when it has one.
    pub fn diff_expanded(&self, index: usize) -> Option<Vec<String>> {
        self.cells[index]
            .diff
            .as_ref()
            .map(inline_diff::Diff::expanded)
    }

    /// The tool a painted diff row targets, when the cell has a diff.
    pub fn diff_target(&self, cell: usize, row: usize) -> Option<usize> {
        self.cells[cell].diff.as_ref()?.target(row)
    }

    /// The palette the cell's cached rows were painted with, if cached.
    pub fn cache_theme(&self, index: usize) -> Option<bool> {
        self.cells[index].cache.as_ref().map(|cache| cache.theme)
    }

    pub fn tool_focus(&self) -> Option<Focus> {
        self.tool_focus
    }

    pub fn group_closed(&self, index: usize) -> bool {
        self.closed_group(index)
    }

    pub fn tool_hidden(&self, index: usize) -> bool {
        self.hidden_tool(index)
    }

    pub fn window_first(&self) -> Option<WindowRow> {
        self.window.first().map(|row| WindowRow {
            index: row.index,
            start: row.start,
            end: row.end,
        })
    }

    pub fn painted_rows(&self) -> impl Iterator<Item = PaintedCell> + '_ {
        self.painted.iter().map(|row| PaintedCell {
            cell: row.cell,
            row: row.row,
        })
    }

    /// Which cells have a measured height, in order.
    pub fn measured(&self) -> impl Iterator<Item = bool> + '_ {
        self.cells.iter().map(|cell| cell.height.is_some())
    }

    pub fn slices(&self) -> impl Iterator<Item = &Slice> + '_ {
        self.cells.iter().map(|cell| &cell.slice)
    }

    /// Seek the scrollbar as a drag would, for a test's setup.
    pub fn seek_to(&mut self, position: usize, maximum: usize) {
        self.seek(position, maximum);
    }
}
