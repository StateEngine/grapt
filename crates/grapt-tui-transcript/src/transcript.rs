//! History cells, transcript view and Markdown render cache:
//! committed source, one draining live tail, and width-keyed visible-cell layout.
//! The cached output key and eviction lifetime follow the pager entry.
mod footer;
use crate::{
    markdown::{self, Markdown},
    palette::Palette,
};
use grapt_client::{
    projection::{Mark, ToolPhase},
    Slice, SliceKind,
};
use ratatui::{
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    Frame,
};
use std::collections::{BTreeSet, VecDeque};

mod anchor;
mod automated;
#[cfg(test)]
mod bounds_tests;
mod compaction;
mod disclosure;
mod inline_diff;
mod input_blocks;
mod mail;
mod notices;
#[cfg(any(test, feature = "test-support"))]
mod observe;
mod prose;
mod raster;
mod scrollbar;
mod stalled;
mod tool_groups;
mod tools;
#[cfg(test)]
mod visibility_tests;
mod window;
#[cfg(any(test, feature = "test-support"))]
pub use disclosure::Focus;
#[cfg(any(test, feature = "test-support"))]
pub use observe::{PaintedCell, WindowRow};

struct Cell {
    slice: Slice,
    observed_at: std::time::Duration,
    expanded: bool,
    diff: Option<inline_diff::Diff>,
    shown: usize,
    cache: Option<CachedOutput>,
    height: Option<(u16, usize)>,
    committed: Vec<LineCell>,
    /// An unsealed partial line drawn once it had been hidden long enough;
    /// never a line boundary, so its line grows in place when the rest arrives.
    provisional: usize,
    /// When the open text's partial line was last seen first hidden.
    hidden_since: Option<std::time::Duration>,
}

/// A sealed assistant line. Appends never change an earlier line boundary.
struct LineCell {
    end: usize,
}

// Keep the rendered output and its
// width/theme key together, invalidating content changes at their owner.
struct CachedOutput {
    width: u16,
    theme: bool,
    rendered: Vec<Line<'static>>,
    row_epochs: Vec<u64>,
    source_end: usize,
    markdown: Option<markdown::Incremental>,
}

impl CachedOutput {
    fn rows(&self, kind: SliceKind) -> &[Line<'static>] {
        // Incremental Markdown retains its reserved heading, but prose never paints it.
        &self.rendered[usize::from(kind == SliceKind::Text)..]
    }
}

#[derive(PartialEq, Eq)]
struct ViewKey {
    revision: u64,
    width: u16,
    height: u16,
    scroll: usize,
    from_top: bool,
    light: bool,
}

#[derive(Clone, Copy)]
struct Window {
    index: usize,
    start: usize,
    end: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct PaintedRow {
    cell: usize,
    row: usize,
    epoch: u64,
}

pub struct Transcript {
    clock: std::time::Duration,
    live_compaction: Option<std::time::Duration>,
    /// Cached answer for [`Transcript::compacting`]'s cell scan, invalidated
    /// whenever the cells change. Interior mutability keeps `compacting` a
    /// read: the paint path and the deadline path both call it.
    compaction_cell: std::cell::Cell<Option<bool>>,
    /// Compaction ids that were already visible when the current turn became
    /// live. A durable `Running` phase in this set is history replayed from an
    /// interrupted turn, so the footer must not announce it as current. Identity
    /// is used rather than a clock reading or a cell index: replay and the turn
    /// start can share a clock tick, and a rebuilt page reuses no indices.
    turn_started: Option<BTreeSet<grapt_client::wire::MessageWireId>>,
    tool_groups: tool_groups::ToolGroups,
    cells: Vec<Cell>,
    window: Vec<Window>,
    painted: Vec<PaintedRow>,
    painted_area: Option<Rect>,
    tool_focus: Option<disclosure::Focus>,
    paint_epoch: u64,
    raster: raster::Raster,
    pending: VecDeque<usize>,
    cached: VecDeque<usize>,
    renderer: Markdown,
    revision: u64,
    view: Option<ViewKey>,
    pub scroll: usize,
    pub from_top: bool,
    pub overflow: Option<(bool, usize)>,
    tail_anchor: Option<anchor::Tail>,
    seek: Option<window::Seek>,
    scrollbar_drag: Option<scrollbar::Drag>,
}

impl Default for Transcript {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Transcript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transcript").finish_non_exhaustive()
    }
}

impl Transcript {
    pub fn new() -> Self {
        Self {
            clock: std::time::Duration::ZERO,
            live_compaction: None,
            compaction_cell: std::cell::Cell::new(None),
            turn_started: None,
            tool_groups: tool_groups::ToolGroups::default(),
            cells: Vec::new(),
            window: Vec::new(),
            painted: Vec::new(),
            painted_area: None,
            tool_focus: None,
            paint_epoch: 0,
            raster: raster::Raster::default(),
            pending: VecDeque::new(),
            cached: VecDeque::new(),
            renderer: Markdown::new(),
            revision: 0,
            view: None,
            scroll: 0,
            from_top: false,
            overflow: None,
            tail_anchor: None,
            seek: None,
            scrollbar_drag: None,
        }
    }

    /// Recovery cannot skip an incomplete latest input and restore an older one.
    pub fn latest_prompt(&self) -> Option<(usize, &str)> {
        let cell = self.cells.iter().rev().find(|cell| {
            matches!(
                cell.slice.kind,
                SliceKind::User | SliceKind::Automated | SliceKind::SeatStartup
            )
        })?;
        (cell.slice.kind == SliceKind::User
            && cell.slice.closed
            && cell.slice.truncated_bytes == 0
            && crate::prompt::valid_prompt(&cell.slice.text))
        .then_some((cell.slice.ordinal, cell.slice.text.as_str()))
    }

    pub fn prompts(&self) -> impl Iterator<Item = &str> {
        self.cells
            .iter()
            .rev()
            .filter(|cell| {
                cell.slice.kind == SliceKind::User
                    && cell.slice.closed
                    && cell.slice.truncated_bytes == 0
            })
            .map(|cell| cell.slice.text.as_str())
    }

    pub fn set_wake(&self, wake: std::sync::mpsc::SyncSender<()>) {
        self.renderer.set_wake(wake);
    }

    pub fn refresh_highlights(&mut self) -> Result<bool, String> {
        if !self.renderer.refresh()? {
            return Ok(false);
        }
        // Heights are unchanged: only the syntax colors arrived.
        for index in self.cached.drain(..) {
            self.cells[index].cache = None;
        }
        Ok(true)
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn update(&mut self, reset: bool, start: usize, rows: Vec<Slice>) -> bool {
        let Some(rows) = self.defer_drag_update(reset, start, rows) else {
            return true;
        };
        self.pin_tail();
        if reset {
            self.seek = None;
            self.overflow = None;
            self.tool_groups.clear();
            self.tool_focus = None;
            self.cells.clear();
            self.compaction_cell.set(None);
            self.pending.clear();
            self.cached.clear();
            self.scroll = 0;
            self.from_top = false;
            self.tail_anchor = None;
        }
        let mut changed = reset;
        let mut tool_changes = Vec::new();
        for (offset, slice) in rows.into_iter().enumerate() {
            let index = start + offset;
            let affects_group = matches!(slice.kind, SliceKind::Tool | SliceKind::Reasoning)
                || self.cells.get(index).is_some_and(|cell| {
                    matches!(cell.slice.kind, SliceKind::Tool | SliceKind::Reasoning)
                });
            if let Some(old) = self.cells.get_mut(index) {
                if old.slice == slice {
                    continue;
                }
                // A compaction can start, finish or fail in place; the cached
                // scan is stale from here on.
                if old.slice.compaction != slice.compaction {
                    self.compaction_cell.set(None);
                }
                if old.slice.tool_id != slice.tool_id
                    || old.slice.compaction.as_ref().map(|c| &c.id)
                        != slice.compaction.as_ref().map(|c| &c.id)
                {
                    old.observed_at = self.clock;
                    if self.tool_focus.is_some_and(|focus| focus.cell() == index) {
                        self.tool_focus = None;
                    }
                    old.expanded = false;
                    old.diff = None;
                }
                // Reasoning is a lifecycle cell; running tools retain their
                // initial name/arguments until their terminal update.
                let tool_start = old.slice.tool_phase == Some(ToolPhase::Streaming)
                    && slice.tool_phase == Some(ToolPhase::Running);
                if (slice.kind == SliceKind::Reasoning
                    || (slice.kind == SliceKind::Tool && !tool_start))
                    && !slice.closed
                {
                    if tools::update_progress(old, &slice) {
                        changed = true;
                        tool_changes.push(index);
                    }
                    continue;
                }
                if slice.kind == SliceKind::Text {
                    let replacement = !slice.text.starts_with(&old.slice.text);
                    let terminal = old.slice.closed != slice.closed;
                    let unchanged_source = old.slice.text == slice.text
                        && old.slice.truncated_bytes == slice.truncated_bytes;
                    if replacement {
                        old.shown = 0;
                        old.provisional = 0;
                        old.hidden_since = None;
                        old.committed.clear();
                        old.cache = None;
                        old.height = None;
                    }
                    old.slice = slice;
                    changed |= replacement || terminal;
                    if terminal {
                        // Closing the same source does not change its Markdown.
                        // A pending final line changes source_end when drained;
                        // highlight results invalidate through their own owner.
                        if !unchanged_source {
                            old.cache = None;
                        }
                        old.height = None;
                    }
                    if ready_line(old) && !self.pending.contains(&index) {
                        self.pending.push_back(index);
                    }
                } else {
                    old.shown = if slice.kind == SliceKind::Reasoning {
                        0
                    } else {
                        slice.text.len()
                    };
                    old.diff = inline_diff::Diff::new(&slice, old.diff.as_ref());
                    old.slice = slice;
                    old.cache = None;
                    old.height = None;
                    changed = true;
                }
            } else if index == self.cells.len() {
                let shown = match slice.kind {
                    SliceKind::Reasoning => 0,
                    SliceKind::Text if !slice.closed => 0,
                    _ => slice.text.len(),
                };
                let committed = if slice.kind == SliceKind::Text {
                    line_cells(&slice.text[..shown])
                } else {
                    Vec::new()
                };
                let cell = Cell {
                    observed_at: self.clock,
                    diff: inline_diff::Diff::new(&slice, None),
                    slice,
                    expanded: false,
                    shown,
                    cache: None,
                    height: None,
                    committed,
                    provisional: 0,
                    hidden_since: None,
                };
                if ready_line(&cell) {
                    self.pending.push_back(index);
                }
                self.cells.push(cell);
                // A new cell may carry a running compaction, or may be the
                // first cell of a turn that makes an existing one current.
                self.compaction_cell.set(None);
                changed = true;
            }
            if affects_group {
                tool_changes.push(index);
            }
        }
        if changed {
            self.update_tool_groups(&tool_changes);
            self.revision = self.revision.saturating_add(1);
        }
        changed
    }

    pub fn recent_rows(&self, limit: usize) -> Vec<Slice> {
        self.cells
            .iter()
            .skip(self.cells.len().saturating_sub(limit))
            .map(|cell| cell.slice.clone())
            .collect()
    }

    pub fn draining(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Drain oldest complete lines first, batching only
    /// under queue pressure. A partial line never schedules another paint.
    pub fn drain(&mut self) -> bool {
        self.pin_tail();
        let mut changed = false;
        let mut budget = 1;
        if self
            .pending
            .front()
            .and_then(|i| self.cells.get(*i))
            .is_some_and(|cell| {
                cell.slice.text[cell.shown..]
                    .bytes()
                    .filter(|b| *b == b'\n')
                    .take(5)
                    .count()
                    > 4
            })
        {
            budget = 8;
        }
        while budget > 0 {
            let Some(&index) = self.pending.front() else {
                break;
            };
            let Some(cell) = self.cells.get_mut(index) else {
                self.pending.pop_front();
                continue;
            };
            let remaining = &cell.slice.text[cell.shown..];
            let end = remaining
                .find('\n')
                .map(|i| cell.shown + i + 1)
                .or_else(|| {
                    (cell.slice.closed && !remaining.is_empty()).then_some(cell.slice.text.len())
                });
            if let Some(end) = end {
                cell.committed.push(LineCell { end });
                cell.shown = end;
                // The cached top-level prefix survives append-only source.
                cell.height = None;
                changed = true;
                budget -= 1;
            }
            if !ready_line(cell) {
                self.pending.pop_front();
            }
        }
        if changed {
            self.revision = self.revision.saturating_add(1);
        }
        changed
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn layout(&mut self, index: usize, width: u16, palette: Palette) -> &[Line<'static>] {
        // Reasoning has no transcript rows; folded children retain their sources.
        if self.cells[index].slice.kind == SliceKind::Reasoning || self.hidden_tool(index) {
            return &[];
        }
        let cell = &self.cells[index];
        let sealed = cell.committed.last().map_or(cell.shown, |line| line.end);
        let end = sealed.max(cell.provisional.min(cell.slice.text.len()));
        // A provisional partial line renders fresh and keeps no incremental
        // state: the next sealed line renders from sealed source only.
        let provisional = end > sealed;
        let valid = self.cells[index].cache.as_ref().is_some_and(|cache| {
            cache.width == width && cache.theme == palette.light && cache.source_end == end
        });
        if !valid {
            if self.compaction(index, width, palette)
                || self.inline_diff(index, width, palette)
                || self.footer(index, width, palette)
                || self.mail(index, width, palette)
                || self.collapsed_automated(index, width, palette)
                || self.collapsed_tool(index, width, palette)
            {
                return self.cells[index]
                    .cache
                    .as_ref()
                    .map(|cache| cache.rendered.as_slice())
                    .unwrap_or_default();
            }
            self.paint_epoch = self.paint_epoch.saturating_add(1);
            let epoch = self.paint_epoch;
            crate::profile::layout(false);
            let old = self.cells[index]
                .cache
                .take()
                .filter(|cache| cache.width == width && cache.theme == palette.light);
            let cell = &self.cells[index];
            if cell.slice.tool_phase == Some(ToolPhase::Streaming) {
                self.cells[index].cache = Some(CachedOutput {
                    width,
                    theme: palette.light,
                    rendered: Vec::new(),
                    row_epochs: Vec::new(),
                    source_end: end,
                    markdown: None,
                });
                return &[];
            }
            let (text, mark_offset) = tools::detail(&cell.slice, &cell.slice.text[..end]);
            let kind = cell.slice.kind;
            let heading = match kind {
                SliceKind::AssistantFooter => "",
                SliceKind::User => "You",
                SliceKind::Automated => "Automated prompt · F8 collapse",
                SliceKind::SeatStartup => "Seat startup prompt · F8 collapse",
                SliceKind::Text => "",
                SliceKind::Reasoning => {
                    if cell.slice.closed {
                        "Reasoning · complete"
                    } else {
                        "Thinking…"
                    }
                }
                SliceKind::Tool => "Tool",
                SliceKind::System => "Session",
                SliceKind::Unsupported => "Unsupported",
            };
            let mut rows = vec![Line::from(Span::styled(
                heading,
                palette
                    .normal()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            ))];
            if kind == SliceKind::Tool {
                rows = self.tool_heading(
                    index,
                    true,
                    width,
                    palette,
                    self.tool_focus == Some(disclosure::Focus::Tool(index)),
                );
            }
            let mut incremental = None;
            let mut row_epochs = Vec::new();
            if input_blocks::is_prompt(kind) {
                rows = input_blocks::rows(kind, text, width, palette);
            } else if kind == SliceKind::System {
                rows = notices::rows(text, width, palette);
            } else if kind == SliceKind::Text {
                let mut state = markdown::Incremental::default();
                if let Some(mut old) = old.filter(|_| !provisional) {
                    if let Some(previous) = old.markdown.take() {
                        rows = old.rendered;
                        row_epochs = old.row_epochs;
                        state = previous;
                    }
                }
                let retained = state.append(
                    &self.renderer,
                    text,
                    prose::width(width),
                    palette,
                    &mut rows,
                );
                row_epochs.truncate(retained);
                incremental = (!provisional).then_some(state);
            } else {
                let mut literal = Vec::new();
                for (i, line) in text.split('\n').enumerate() {
                    let color = match cell.slice.marks.get(i + mark_offset) {
                        Some(Mark::Insert) => palette.good,
                        Some(Mark::Delete) => palette.bad,
                        Some(Mark::Header) => palette.accent,
                        _ => palette.text,
                    };
                    literal.push(Line::styled(
                        markdown::literal(line),
                        palette.normal().fg(color),
                    ));
                }
                let detail_width = if kind == SliceKind::Tool {
                    width.saturating_sub(3)
                } else {
                    width
                };
                let mut body = markdown::wrap(literal, detail_width);
                if kind == SliceKind::Tool {
                    for line in &mut body {
                        line.spans
                            .insert(0, Span::styled("  │", palette.normal().fg(palette.muted)));
                    }
                }
                rows.extend(body);
            }
            if cell.slice.truncated_bytes > 0 {
                rows.push(Line::styled(
                    format!("[{} source bytes omitted]", cell.slice.truncated_bytes),
                    palette.normal().fg(palette.bad),
                ));
            }
            if kind != SliceKind::Tool || self.tool_separator(index) {
                rows.push(Line::default());
            }
            row_epochs.resize(rows.len(), epoch);
            self.cells[index].height =
                Some((width, rows.len() - usize::from(kind == SliceKind::Text)));
            self.cells[index].cache = Some(CachedOutput {
                width,
                theme: palette.light,
                rendered: rows,
                row_epochs,
                source_end: end,
                markdown: incremental,
            });
            self.remember_cache(index);
        }
        match &self.cells[index].cache {
            Some(cache) => cache.rows(self.cells[index].slice.kind),
            None => &[],
        }
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn height(&mut self, index: usize, width: u16) -> usize {
        if self.hidden_tool(index) {
            return 0;
        }
        let cell = &self.cells[index];
        if cell.slice.kind == SliceKind::Reasoning {
            return 0;
        }
        if cell.slice.compaction.is_some() {
            self.compaction(index, width, Palette::new(false));
            return self.cells[index]
                .cache
                .as_ref()
                .map_or(1, |cache| cache.rendered.len());
        }
        if let Some(diff) = &cell.diff {
            return diff.height();
        }
        if mail::envelope(&cell.slice).is_some() {
            if let Some((cached_width, height)) = cell.height {
                if cached_width == width {
                    return height;
                }
            }
            self.mail(index, width, Palette::new(false));
            return self.cells[index]
                .cache
                .as_ref()
                .map_or(0, |cache| cache.rendered.len());
        }
        if self.closed_group(index) {
            return self
                .tool_heading(index, false, width, Palette::new(false), false)
                .len();
        }
        if cell.slice.kind == SliceKind::AssistantFooter {
            if let Some((cached_width, height)) = cell.height {
                if cached_width == width {
                    return height;
                }
            }
            let height = cell.slice.footer.as_deref().map_or(0, |footer| {
                crate::turn_footer::reply(footer, width, Palette::new(false)).len()
            });
            self.cells[index].height = Some((width, height));
            return height;
        }
        if !cell.expanded {
            if matches!(
                cell.slice.kind,
                SliceKind::Automated | SliceKind::SeatStartup
            ) {
                return 1;
            }
            if cell.slice.kind == SliceKind::Tool {
                return self
                    .tool_heading(index, false, width, Palette::new(false), false)
                    .len();
            }
        }
        if let Some((cached_width, height)) = cell.height {
            if cached_width == width {
                return height;
            }
        }
        crate::profile::layout(true);
        if cell.slice.tool_phase == Some(ToolPhase::Streaming) {
            return self
                .tool_heading(index, cell.expanded, width, Palette::new(false), false)
                .len();
        }
        let end = cell.committed.last().map_or(cell.shown, |line| line.end);
        let text = &cell.slice.text[..end];
        if input_blocks::is_prompt(cell.slice.kind) {
            let height = input_blocks::rows(cell.slice.kind, text, width, Palette::new(false))
                .len()
                + 1
                + usize::from(cell.slice.truncated_bytes > 0);
            self.cells[index].height = Some((width, height));
            return height;
        }
        if cell.slice.kind == SliceKind::System {
            let height = notices::rows(text, width, Palette::new(false)).len()
                + 1
                + usize::from(cell.slice.truncated_bytes > 0);
            self.cells[index].height = Some((width, height));
            return height;
        }
        let (text, _) = tools::detail(&cell.slice, text);
        let body = if cell.slice.kind == SliceKind::Text {
            self.renderer.height(text, prose::width(width))
        } else {
            markdown::wrap(
                text.split('\n')
                    .map(|line| Line::raw(markdown::literal(line)))
                    .collect(),
                if cell.slice.kind == SliceKind::Tool {
                    width.saturating_sub(3)
                } else {
                    width
                },
            )
            .len()
        };
        let heading = if cell.slice.kind == SliceKind::Tool {
            self.tool_heading(index, true, width, Palette::new(false), false)
                .len()
        } else if cell.slice.kind == SliceKind::Text {
            0
        } else {
            1
        };
        let separator = cell.slice.kind != SliceKind::Tool || self.tool_separator(index);
        let height =
            body + heading + usize::from(separator) + usize::from(cell.slice.truncated_bytes > 0);
        self.cells[index].height = Some((width, height));
        height
    }

    /// Full UI frames start with a cleared buffer; stream frames retain the canvas.
    pub fn paint(&mut self, frame: &mut Frame<'_>, area: Rect, palette: Palette) {
        self.paint_rows(frame, area, palette, false, None);
    }

    pub fn paint_changed(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        palette: Palette,
        previous: Option<&ratatui::buffer::Buffer>,
    ) {
        self.paint_rows(frame, area, palette, true, previous);
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn paint_rows(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        palette: Palette,
        retained: bool,
        canvas: Option<&ratatui::buffer::Buffer>,
    ) {
        // A live advisory phase has no durable message identity. Reserve one
        // row without inserting a synthetic history record or disclosure hit.
        let live = self.live_compaction.filter(|_| {
            !self.cells.iter().any(|cell| {
                cell.slice
                    .compaction
                    .as_ref()
                    .is_some_and(|c| c.phase == grapt_client::wire::CompactionPhase::Running)
            })
        });
        let area = if let Some(observed) = live.filter(|_| area.height > 0) {
            let row = Rect {
                y: area.bottom().saturating_sub(1),
                height: 1,
                ..area
            };
            frame.render_widget(
                Line::styled(
                    tool_groups::fit(
                        &format!(
                            "◇ Compacting… · observed {}s",
                            self.clock.saturating_sub(observed).as_secs()
                        ),
                        area.width,
                    ),
                    palette.normal().fg(palette.muted),
                ),
                row,
            );
            Rect {
                height: area.height.saturating_sub(1),
                ..area
            }
        } else {
            area
        };
        self.window(area.width, area.height, palette);
        self.raster.begin(area, palette);
        let damage = retained && self.painted_area == Some(area);
        let previous = self.painted.len();
        self.painted_area = Some(area);
        if area.is_empty() {
            self.painted.clear();
            self.raster.finish();
            return;
        }
        let mut shown = 0;
        for position in 0..self.window.len() {
            let slot = if self.from_top {
                position
            } else {
                self.window.len() - position - 1
            };
            let Window { index, start, end } = self.window[slot];
            self.layout(index, area.width, palette);
            let kind = self.cells[index].slice.kind;
            let first = self.starts_reply(index);
            if let Some(cache) = &self.cells[index].cache {
                for row in start..end {
                    let line = || {
                        let source = &cache.rows(kind)[row];
                        if kind == SliceKind::Text {
                            std::borrow::Cow::Owned(prose::line(
                                source, row, first, area.width, palette,
                            ))
                        } else {
                            std::borrow::Cow::Borrowed(source)
                        }
                    };
                    let key = PaintedRow {
                        cell: index,
                        row,
                        epoch: cache.row_epochs[row + usize::from(kind == SliceKind::Text)],
                    };
                    let changed = self.painted.get(shown) != Some(&key);
                    let line_area = Rect {
                        y: area.y.saturating_add(shown as u16),
                        height: 1,
                        ..area
                    };
                    if !damage {
                        frame.render_widget(line().as_ref(), line_area);
                        self.raster.keep(key, line_area);
                    } else if changed {
                        self.raster.paint(
                            frame.buffer_mut(),
                            line_area,
                            key,
                            line().as_ref(),
                            palette,
                            canvas,
                        );
                    } else {
                        self.raster.keep(key, line_area);
                    }
                    if let Some(old) = self.painted.get_mut(shown) {
                        *old = key;
                    } else {
                        self.painted.push(key);
                    }
                    shown += 1;
                }
            }
        }
        if damage {
            for row in shown..previous.min(usize::from(area.height)) {
                clear_row(
                    frame,
                    Rect {
                        y: area.y.saturating_add(row as u16),
                        height: 1,
                        ..area
                    },
                    palette,
                );
            }
        }
        self.painted.truncate(shown);
        self.raster.finish();
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn visible(&mut self, width: u16, height: u16, palette: Palette) -> Vec<Line<'static>> {
        self.window(width, height, palette);
        let mut rows = Vec::new();
        for position in 0..self.window.len() {
            let slot = if self.from_top {
                position
            } else {
                self.window.len() - position - 1
            };
            let Window { index, start, end } = self.window[slot];
            let kind = self.cells[index].slice.kind;
            let first = self.starts_reply(index);
            rows.extend(
                self.layout(index, width, palette)[start..end]
                    .iter()
                    .enumerate()
                    .map(|(offset, line)| {
                        if kind == SliceKind::Text {
                            prose::line(line, start + offset, first, width, palette)
                        } else {
                            line.clone()
                        }
                    }),
            );
        }
        rows
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn top(&mut self) {
        self.scroll = 0;
        self.from_top = true;
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn painted_cells(&self) -> usize {
        self.cells
            .iter()
            .filter(|cell| cell.cache.is_some())
            .count()
    }
}

fn ready_line(cell: &Cell) -> bool {
    cell.slice.kind == SliceKind::Text
        && cell.shown < cell.slice.text.len()
        && (cell.slice.closed || cell.slice.text[cell.shown..].contains('\n'))
}

fn line_cells(text: &str) -> Vec<LineCell> {
    let mut end = 0;
    text.split_inclusive('\n')
        .map(|line| {
            end += line.len();
            LineCell { end }
        })
        .collect()
}

#[cfg(test)]
mod tests;

fn clear_row(frame: &mut Frame<'_>, area: Rect, palette: Palette) {
    use ratatui::widgets::{Block, Clear};
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(palette.normal()), area);
}
