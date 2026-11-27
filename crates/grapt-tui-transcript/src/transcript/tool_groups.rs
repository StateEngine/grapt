//! Fold mechanics between prose blocks, following the old TUI's ExplorationSummary.
//! Group disclosure is separate from each original tool's retained output.
use super::*;
use grapt_client::wire::ToolCallWireId;
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    time::Duration,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

mod live;
const MAX_CALLS: usize = 12;

#[derive(Default)]
pub(super) struct ToolGroups {
    pub(super) runs: BTreeMap<usize, Group>,
    active: BTreeSet<usize>,
    phase: usize,
    live: bool,
    open: Option<usize>,
}

pub(super) struct Group {
    end: usize,
    id: Option<ToolCallWireId>,
    label: String,
    error: Option<String>,
    running: bool,
    pub(super) expanded: bool,
}

fn bridge(cell: &Cell) -> bool {
    cell.diff.is_none() && matches!(cell.slice.kind, SliceKind::Tool | SliceKind::Reasoning)
}

impl ToolGroups {
    // Only changed tool/reasoning neighborhoods are visited. A prose append
    // does no group work; stable history keeps both its layouts and heights.
    fn update(&mut self, cells: &[Cell], changes: &[usize]) -> Vec<usize> {
        let mut invalidated = Vec::new();
        let mut covered = 0;
        for &index in changes {
            if index < covered || index >= cells.len() {
                continue;
            }
            let mut from = index;
            while from > 0 && bridge(&cells[from - 1]) {
                from -= 1;
            }
            let mut end = index + 1;
            while end < cells.len() && bridge(&cells[end]) {
                end += 1;
            }
            covered = end;
            let keys: Vec<_> = self
                .runs
                .range(from..end)
                .map(|(&start, _)| start)
                .collect();
            let mut previous = BTreeMap::new();
            for start in keys {
                if let Some(group) = self.runs.remove(&start) {
                    previous.insert(start, group);
                }
                self.active.remove(&start);
                invalidated.push(start);
            }
            let mut cursor = from;
            while cursor < end {
                if !bridge(&cells[cursor]) || cells[cursor].slice.kind != SliceKind::Tool {
                    cursor += 1;
                    continue;
                }
                let start = cursor;
                let mut stop = cursor + 1;
                let mut calls = 0;
                while cursor < end && bridge(&cells[cursor]) {
                    if cells[cursor].slice.kind == SliceKind::Tool {
                        if calls == MAX_CALLS {
                            break;
                        }
                        calls += 1;
                        stop = cursor + 1;
                    }
                    cursor += 1;
                }
                let (mut label, running, error) = summarize(
                    cells[start..stop]
                        .iter()
                        .filter(|cell| cell.slice.kind == SliceKind::Tool)
                        .map(|cell| &cell.slice),
                );
                if calls == 1 {
                    if let Some(tool) = &cells[start].slice.tool_summary {
                        label.clone_from(&tool.title);
                        if cells[start].slice.tool_phase == Some(ToolPhase::Running)
                            && !tool.result.is_empty()
                        {
                            label.push_str(" · ");
                            label.push_str(&tool.result);
                        }
                    }
                }
                let id = cells[start].slice.tool_id.clone();
                let expanded = previous.get(&start).is_some_and(|group| {
                    group.id == id
                        && if group.end == start + 1 {
                            cells[start].expanded
                        } else {
                            group.expanded
                        }
                });
                if previous.get(&start).map(|group| group.end) != Some(stop) {
                    if let Some(group) = previous.get(&start) {
                        invalidated.push(group.end - 1);
                    }
                    invalidated.push(stop - 1);
                }
                previous.remove(&start);
                self.runs.insert(
                    start,
                    Group {
                        end: stop,
                        id,
                        label,
                        running,
                        error,
                        expanded,
                    },
                );
                if running {
                    self.active.insert(start);
                }
                invalidated.push(start);
            }
            // A removed or merged run can leave its old separator in a child.
            invalidated.extend(previous.into_values().map(|group| group.end - 1));
        }
        invalidated
    }

    pub(super) fn span(&self, index: usize) -> Option<Range<usize>> {
        let (&start, group) = self.runs.range(..=index).next_back()?;
        (index < group.end).then_some(start..group.end)
    }

    pub(super) fn expanded_ids(&self) -> BTreeSet<ToolCallWireId> {
        self.runs
            .values()
            .filter(|group| group.expanded)
            .filter_map(|group| group.id.clone())
            .collect()
    }
    pub(super) fn restore_expanded(&mut self, ids: &BTreeSet<ToolCallWireId>) {
        for group in self.runs.values_mut() {
            group.expanded = group.id.as_ref().is_some_and(|id| ids.contains(id));
        }
    }
}

impl Transcript {
    pub(super) fn hidden_tool(&self, index: usize) -> bool {
        self.tool_groups
            .span(index)
            .is_some_and(|run| run.start != index && !self.tool_groups.runs[&run.start].expanded)
    }
    pub(super) fn closed_group(&self, index: usize) -> bool {
        self.tool_groups
            .runs
            .get(&index)
            .is_some_and(|group| group.end != index + 1 && !group.expanded)
    }
    pub(super) fn group_heading(
        &self,
        index: usize,
        width: u16,
        palette: Palette,
    ) -> Vec<Line<'static>> {
        let Some(group) = self.tool_groups.runs.get(&index) else {
            return vec![];
        };
        if group.end == index + 1 {
            return vec![];
        }
        let disclosure = if group.expanded { "▾" } else { "▸" };
        let running = if group.running {
            let glyph = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][self.tool_groups.phase];
            format!(" · running {glyph}")
        } else {
            String::new()
        };
        let mut style = palette.normal().fg(palette.muted);
        if group.error.is_some() {
            style = style
                .add_modifier(Modifier::DIM)
                .remove_modifier(Modifier::BOLD);
        }
        if self.tool_focus == Some(disclosure::Focus::Group(index)) {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        let mut rows = vec![Line::styled(
            fit(&format!("● {}{running} {disclosure}", group.label), width),
            style,
        )];
        if let Some(error) = &group.error {
            rows.push(Line::styled(
                fit(&format!("  └ {error}"), width),
                palette
                    .normal()
                    .fg(palette.muted)
                    .add_modifier(Modifier::DIM)
                    .remove_modifier(Modifier::BOLD),
            ));
        }
        if !group.expanded && group.end != index + 1 && self.tool_groups.open == Some(index) {
            rows.extend(self.live_tool_titles(index, group.end, width, palette));
        }
        rows
    }
    // One separator belongs to the visible end of the group, never to each
    // preview. The same boundary drives both painted rows and cold heights.
    pub(super) fn tool_separator(&self, index: usize) -> bool {
        self.closed_group(index)
            || self
                .tool_groups
                .span(index)
                .is_some_and(|run| run.end == index + 1)
    }
    pub(super) fn tool_heading(
        &self,
        index: usize,
        expanded: bool,
        width: u16,
        palette: Palette,
        focused: bool,
    ) -> Vec<Line<'static>> {
        let mut rows = self.group_heading(index, width, palette);
        if !self.closed_group(index) {
            rows.extend(tools::summary(
                &self.cells[index].slice,
                expanded,
                width,
                palette,
                focused,
            ));
        }
        let body = expanded
            && self.cells[index].slice.tool_phase != Some(ToolPhase::Streaming)
            && !self.closed_group(index);
        if !body && self.tool_separator(index) {
            rows.push(Line::default());
        }
        rows
    }
    pub fn toggle_group(&mut self, index: usize) {
        let Some(group) = self.tool_groups.runs.get_mut(&index) else {
            return;
        };
        group.expanded = !group.expanded;
        self.cells[index].cache = None;
        self.cells[index].height = None;
        self.revision = self.revision.saturating_add(1);
    }
    /// Explicit disclosure opens retained command/output for every tool in this run.
    /// Group visibility and each tool's state still have their existing owners.
    pub(super) fn disclose_group(&mut self, index: usize) {
        self.toggle_group(index);
        let Some(group) = self.tool_groups.runs.get(&index) else {
            return;
        };
        for cell in &mut self.cells[index..group.end] {
            if cell.slice.kind == SliceKind::Tool {
                cell.expanded = group.expanded;
                cell.cache = None;
                cell.height = None;
            }
        }
    }
    /// The host passes its existing monotonic clock. Idle tools schedule no paints.
    pub fn animate_tools(&mut self, elapsed: Duration) -> bool {
        let phase = ((elapsed.as_millis() / 100) % 10) as usize;
        if phase == self.tool_groups.phase {
            return false;
        }
        self.tool_groups.phase = phase;
        let changed = self.tool_groups.active.iter().any(|index| {
            self.tool_groups
                .runs
                .get(index)
                .is_some_and(|group| group.end != *index + 1)
        });
        if changed {
            self.paint_epoch = self.paint_epoch.saturating_add(1);
        }
        for &start in &self.tool_groups.active {
            let heading = self.cells[start]
                .cache
                .as_ref()
                .map(|cache| self.group_heading(start, cache.width, Palette::new(cache.theme)));
            if let Some((cache, heading)) = self.cells[start].cache.as_mut().zip(heading) {
                for ((row, epoch), replacement) in cache
                    .rendered
                    .iter_mut()
                    .zip(&mut cache.row_epochs)
                    .zip(heading)
                {
                    *row = replacement;
                    *epoch = self.paint_epoch;
                }
            }
        }
        if changed {
            self.revision = self.revision.saturating_add(1);
        }
        changed
    }
    pub fn tools_running(&self) -> bool {
        !self.tool_groups.active.is_empty()
    }

    /// The newest not-yet-finished tool call and how long IT has been running.
    ///
    /// `active` holds group START indices, and a group can span a completed
    /// call followed by a running one; reporting the start would name the
    /// finished tool and show its age. So walk forward from each active group
    /// and take the last call still Running.
    pub fn running_tool_elapsed(&self) -> Option<(&str, std::time::Duration)> {
        let index = self.tool_groups.active.iter().rev().find_map(|start| {
            let span = self.tool_groups.span(*start)?;
            let offset = self.cells[span.clone()]
                .iter()
                .rposition(|cell| cell.slice.tool_phase == Some(ToolPhase::Running))?;
            Some(span.start + offset)
        })?;
        let cell = self.cells.get(index)?;
        let title = cell.slice.tool_summary.as_ref()?.title.as_str();
        Some((title, self.clock.saturating_sub(cell.observed_at)))
    }
    pub(super) fn update_tool_groups(&mut self, changes: &[usize]) {
        let mut invalidated = self.tool_groups.update(&self.cells, changes);
        invalidated.extend(self.tool_groups.refresh_open(&self.cells));
        for index in invalidated {
            if let Some(cell) = self.cells.get_mut(index) {
                cell.cache = None;
                cell.height = None;
            }
        }
    }
}

fn summarize<'a>(slices: impl Iterator<Item = &'a Slice>) -> (String, bool, Option<String>) {
    let mut counts = [0usize; 9];
    let mut running = false;
    let mut error = None;
    for slice in slices {
        let name = slice
            .tool_summary
            .as_ref()
            .map(|tool| tool.name.as_str())
            .unwrap_or_default();
        let category = match name {
            "shell" | "exec_command" => 0,
            "read" | "read_lines" => 1,
            "grep" => 2,
            "glob" | "list" => 3,
            "write_stdin" => match slice.tool_summary.as_ref().and_then(|tool| tool.stdin) {
                Some(grapt_client::projection::StdinAction::Wait) => 4,
                Some(grapt_client::projection::StdinAction::Write) => 5,
                None => 8,
            },
            "update_plan" => 6,
            name if name.starts_with("stateengine_") => 7,
            _ => 8,
        };
        counts[category] += 1;
        running |= matches!(
            slice.tool_phase,
            Some(ToolPhase::Streaming | ToolPhase::Running)
        );
        if slice.tool_phase == Some(ToolPhase::Error) && error.is_none() {
            let tool = slice.tool_summary.as_ref();
            let title = tool
                .map(|tool| tool.title.as_str())
                .filter(|title| !title.is_empty())
                .unwrap_or("Tool");
            let message = tool
                .and_then(|tool| tool.error.as_deref())
                .and_then(|text| text.lines().next())
                .filter(|line| !line.is_empty())
                .unwrap_or("expand for details");
            error = Some(format!("{title} failed: {message}"));
        }
    }
    let mut labels = Vec::new();
    for (category, count) in counts
        .into_iter()
        .enumerate()
        .filter(|(_, count)| *count > 0)
    {
        let noun = |singular, plural| if count == 1 { singular } else { plural };
        labels.push(match category {
            0 => format!("ran {count} shell {}", noun("command", "commands")),
            1 => format!("read {count} {}", noun("file", "files")),
            2 => format!("searched {count} {}", noun("pattern", "patterns")),
            3 => format!("listed {count} {}", noun("path", "paths")),
            4 => format!("waited on output {count} {}", noun("time", "times")),
            5 => format!("wrote input {count} {}", noun("time", "times")),
            6 if count == 1 => "updated plan".to_owned(),
            6 => format!("updated plan {count} times"),
            7 if count == 1 => "called stateengine".to_owned(),
            7 => format!("called stateengine {count} times"),
            _ => format!("called {count} {}", noun("tool", "tools")),
        });
    }
    let mut label = labels.join(", ");
    if let Some(first) = label.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    (label, running, error)
}

pub(super) fn fit(text: &str, width: u16) -> String {
    let limit = usize::from(width);
    if text.width() <= limit {
        return text.to_owned();
    }
    if limit == 0 {
        return String::new();
    }
    let mut shown = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        if used + grapheme.width() >= limit {
            break;
        }
        shown.push_str(grapheme);
        used += grapheme.width();
    }
    shown.push('…');
    shown
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod stdin_frames;
