// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// Modified by Grapt: bounded native activity groups retain all original tool calls.
//! Recent calls belong to the open execution group, including between tool runs.
use super::*;

impl ToolGroups {
    pub(in crate::transcript) fn clear(&mut self) {
        self.runs.clear();
        self.active.clear();
        self.open = None;
    }

    pub(super) fn refresh_open(&mut self, cells: &[Cell]) -> Vec<usize> {
        let open = self.runs.last_key_value().and_then(|(&start, group)| {
            (self.live
                && cells[group.end..]
                    .iter()
                    .all(|cell| cell.slice.kind == SliceKind::Reasoning))
            .then_some(start)
        });
        if open == self.open {
            return vec![];
        }
        let previous = std::mem::replace(&mut self.open, open);
        previous.into_iter().chain(open).collect()
    }
}

impl Transcript {
    /// Use the backend's execution state; completed calls remain live between model steps.
    pub fn observe_running(&mut self, running: bool) -> bool {
        if self.tool_groups.live == running {
            return false;
        }
        self.pin_tail();
        self.tool_groups.live = running;
        // A turn that becomes live owns nothing that is already on screen; a
        // turn that ends leaves no current-turn fact behind.
        self.turn_started = running.then(|| {
            self.cells
                .iter()
                .filter_map(|cell| cell.slice.compaction.as_ref().map(|c| c.id.clone()))
                .collect()
        });
        self.compaction_cell.set(None);
        let changed = self.tool_groups.refresh_open(&self.cells);
        for index in &changed {
            self.cells[*index].cache = None;
            self.cells[*index].height = None;
        }
        if !changed.is_empty() {
            self.revision = self.revision.saturating_add(1);
        }
        !changed.is_empty()
    }

    pub(super) fn live_tool_titles(
        &self,
        start: usize,
        end: usize,
        width: u16,
        palette: Palette,
    ) -> Vec<Line<'static>> {
        let recent = grouped_calls(&self.cells[start..end]);
        recent[recent.len().saturating_sub(3)..]
            .iter()
            .map(|&(cell, count)| {
                let title = cell
                    .slice
                    .tool_summary
                    .as_ref()
                    .map_or("Tool", |tool| tool.title.as_str());
                let style = if cell.slice.tool_phase == Some(ToolPhase::Error) {
                    palette
                        .normal()
                        .fg(palette.muted)
                        .add_modifier(Modifier::DIM)
                        .remove_modifier(Modifier::BOLD)
                } else {
                    palette.normal().fg(palette.text)
                };
                let progress = cell
                    .slice
                    .tool_summary
                    .as_ref()
                    .filter(|_| cell.slice.tool_phase == Some(ToolPhase::Running))
                    .map(|tool| tool.result.as_str())
                    .filter(|message| !message.is_empty());
                let title = progress
                    .map(|message| format!("{title} · {message}"))
                    .unwrap_or_else(|| title.into());
                let title = if count > 1 {
                    format!("{count}× {title}")
                } else {
                    title
                };
                Line::styled(fit(&format!("  ● {title}"), width), style)
            })
            .collect()
    }
}

// Only consecutive successful waits on the same known terminal are combined.
// The original cells and their output remain available through disclosure.
pub(super) fn grouped_calls(cells: &[Cell]) -> Vec<(&Cell, usize)> {
    let mut groups: Vec<(&Cell, usize)> = Vec::with_capacity(MAX_CALLS);
    for cell in cells
        .iter()
        .filter(|cell| cell.slice.kind == SliceKind::Tool)
    {
        if let Some((previous, count)) = groups.last_mut() {
            if same_wait(previous, cell) {
                *previous = cell;
                *count = count.saturating_add(1);
                continue;
            }
        }
        groups.push((cell, 1));
    }
    groups
}

fn same_wait(previous: &Cell, next: &Cell) -> bool {
    if previous.slice.tool_phase != Some(ToolPhase::Completed)
        || next.slice.tool_phase != Some(ToolPhase::Completed)
    {
        return false;
    }
    let (Some(previous), Some(next)) = (&previous.slice.tool_summary, &next.slice.tool_summary)
    else {
        return false;
    };
    previous.stdin == Some(grapt_client::projection::StdinAction::Wait)
        && next.stdin == previous.stdin
        && previous.terminal_session.is_some()
        && previous.terminal_session == next.terminal_session
        && previous.title == next.title
}
