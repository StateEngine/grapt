//! Tool disclosure uses the transcript's last painted rows, never a second layout.
// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// link_at and interaction-key ownership, at 33d306d457a4632b22bebf214e0c25d91f90f6e8.
use super::*;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Position;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Group(usize),
    Tool(usize),
    Compaction(usize),
    Mail(usize),
    Diff(usize, usize),
}
impl Focus {
    pub(super) fn cell(self) -> usize {
        match self {
            Self::Group(index)
            | Self::Tool(index)
            | Self::Compaction(index)
            | Self::Mail(index)
            | Self::Diff(index, _) => index,
        }
    }
}

impl Transcript {
    pub fn invalidate_hits(&mut self) {
        self.painted_area = None;
    }

    fn current_hits(&self) -> Option<Rect> {
        let area = self.painted_area?;
        let view = self.view.as_ref()?;
        (view.revision == self.revision
            && view.scroll == self.scroll
            && view.from_top == self.from_top
            && view.width == area.width)
            .then_some(area)
    }

    fn summary_row(&self, row: PaintedRow, width: u16) -> Option<Focus> {
        let cell = self.cells.get(row.cell)?;
        if mail::envelope(&cell.slice).is_some() {
            return (row.row == 0).then_some(Focus::Mail(row.cell));
        }
        if let Some(info) = &cell.slice.compaction {
            return (row.row == 0 && info.phase != grapt_client::wire::CompactionPhase::Running)
                .then_some(Focus::Compaction(row.cell));
        }
        if cell.slice.kind != SliceKind::Tool {
            return None;
        }
        if let Some(diff) = &cell.diff {
            return diff.target(row.row).map(|file| Focus::Diff(row.cell, file));
        }
        let group_rows = self
            .group_heading(row.cell, width, Palette::new(false))
            .len();
        if row.row < group_rows {
            return Some(Focus::Group(row.cell));
        }
        (!self.closed_group(row.cell) && row.row == group_rows).then_some(Focus::Tool(row.cell))
    }

    pub fn click_tool(&mut self, column: u16, row: u16) -> bool {
        let Some(area) = self
            .current_hits()
            .filter(|area| area.contains(Position::new(column, row)))
        else {
            return false;
        };
        let Some(painted) = self.painted.get(usize::from(row - area.y)).copied() else {
            return false;
        };
        let Some(target) = self.summary_row(painted, area.width) else {
            return false;
        };
        self.clear_tool_focus();
        self.toggle_disclosure(target);
        true
    }

    pub fn toggle_tool(&mut self, index: usize) {
        if !self.cells.get(index).is_some_and(|cell| {
            cell.slice.kind == SliceKind::Tool
                || mail::envelope(&cell.slice).is_some()
                || cell
                    .slice
                    .compaction
                    .as_ref()
                    .is_some_and(|c| c.phase != grapt_client::wire::CompactionPhase::Running)
        }) {
            return;
        }
        self.pin_disclosure();
        let cell = &mut self.cells[index];
        cell.expanded = !cell.expanded;
        cell.cache = None;
        cell.height = None;
        self.revision = self.revision.saturating_add(1);
    }

    pub(super) fn pin_disclosure(&mut self) {
        // Keep the clicked header on screen even when its output exceeds a viewport.
        // Reuse the bounded height path used by durable reading-anchor restoration.
        if let Some((first, area)) = self.painted.first().copied().zip(self.painted_area) {
            self.scroll = (0..first.cell)
                .map(|i| self.height(i, area.width))
                .sum::<usize>()
                .saturating_add(first.row);
            self.from_top = true;
            self.tail_anchor = None;
        }
    }
    fn toggle_disclosure(&mut self, target: Focus) {
        match target {
            Focus::Group(index) => {
                self.pin_disclosure();
                self.disclose_group(index);
            }
            Focus::Tool(index) | Focus::Compaction(index) | Focus::Mail(index) => {
                self.toggle_tool(index)
            }
            Focus::Diff(index, file) => self.toggle_diff(index, file),
        }
    }

    fn focus_tool(&mut self, index: Option<Focus>) {
        if self.tool_focus == index {
            return;
        }
        for i in self.tool_focus.into_iter().chain(index) {
            if let Some(cell) = self.cells.get_mut(i.cell()) {
                cell.cache = None;
            }
        }
        self.tool_focus = index;
        self.revision = self.revision.saturating_add(1);
    }

    pub fn clear_tool_focus(&mut self) {
        self.focus_tool(None);
    }
    pub fn tools_focused(&self) -> bool {
        self.tool_focus.is_some()
    }

    /// Quiet accessibility path: BackTab focuses summaries; Enter/Space discloses.
    /// Mouse activation never enters this keyboard focus mode.
    pub fn tool_key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Char('o')
            && key.modifiers == crossterm::event::KeyModifiers::CONTROL
        {
            let target = self
                .tool_focus
                .filter(|f| matches!(f, Focus::Diff(..) | Focus::Compaction(_)))
                .or_else(|| {
                    self.painted.iter().rev().find_map(|row| {
                        let cell = self.cells.get(row.cell)?;
                        if cell.slice.compaction.as_ref().is_some_and(|c| {
                            c.phase != grapt_client::wire::CompactionPhase::Running
                        }) {
                            return Some(Focus::Compaction(row.cell));
                        }
                        let diff = cell.diff.as_ref()?;
                        // The most recently visible file owns Ctrl+O, even when
                        // its heading has scrolled above the viewport.
                        let file = (0..=row.row).rev().find_map(|r| diff.target(r))?;
                        Some(Focus::Diff(row.cell, file))
                    })
                });
            if let Some(target) = target {
                self.toggle_disclosure(target);
                return true;
            }
        }

        if key.code == KeyCode::BackTab || (self.tool_focus.is_some() && key.code == KeyCode::Tab) {
            let Some(area) = self.painted_area else {
                return true;
            };
            let mut headers = Vec::new();
            for row in &self.painted {
                if let Some(target) = self
                    .summary_row(*row, area.width)
                    .filter(|target| headers.last() != Some(target))
                {
                    headers.push(target);
                }
            }
            if headers.is_empty() {
                self.clear_tool_focus();
                return false;
            }
            let current = self
                .tool_focus
                .and_then(|i| headers.iter().position(|item| *item == i));
            let index = if key.code == KeyCode::BackTab {
                current.map_or(headers.len() - 1, |i| {
                    (i + headers.len() - 1) % headers.len()
                })
            } else {
                current.map_or(0, |i| (i + 1) % headers.len())
            };
            self.focus_tool(Some(headers[index]));
            return true;
        }
        let Some(index) = self.tool_focus else {
            return false;
        };
        if matches!(key.code, KeyCode::Enter | KeyCode::Char(' ')) && key.modifiers.is_empty() {
            self.toggle_disclosure(index);
            return true;
        }
        self.clear_tool_focus();
        key.code == KeyCode::Esc
    }
}
