//! Compaction uses the same disclosure state, hit geometry and keyboard owner as diffs.
// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// stable activity identities retain compact/expanded presentation across replay.
use super::*;
use grapt_client::wire::CompactionPhase;
use std::time::Duration;
impl Transcript {
    pub fn observe_compacting(&mut self, active: bool) -> bool {
        if active == self.live_compaction.is_some() {
            return false;
        }
        self.live_compaction = active.then_some(self.clock);
        self.revision = self.revision.saturating_add(1);
        true
    }
    /// Whether a compaction is in progress in the CURRENT turn.
    ///
    /// The live advisory phase is the authoritative fact. A cell scan is only a
    /// fallback for a durable phase that arrives without it, and it must be
    /// scoped to this turn: a `Running` cell replayed from a session whose
    /// compaction was interrupted was already visible BEFORE this turn began, so
    /// counting it made the next ordinary prompt say "Compacting…" again.
    ///
    /// The boundary is the set of ids visible at turn start, not a clock reading
    /// (replay and the turn start can share a tick) nor a cell index (a rebuilt
    /// page renames positions).
    pub fn compacting(&self) -> bool {
        if self.live_compaction.is_some() {
            return true;
        }
        let Some(previously_visible) = &self.turn_started else {
            return false;
        };
        if let Some(cached) = self.compaction_cell.get() {
            return cached;
        }
        let running = self.cells.iter().any(|cell| {
            cell.slice.compaction.as_ref().is_some_and(|c| {
                c.phase == CompactionPhase::Running && !previously_visible.contains(&c.id)
            })
        });
        // Cells are append-only within a turn apart from cache invalidation, so
        // the answer is stable until something invalidates it.
        self.compaction_cell.set(Some(running));
        running
    }
    /// Timing is supplied by the main input adapter; history timestamps do not invent a duration.
    pub fn tick_compactions(&mut self, now: Duration) -> bool {
        let tick = self.clock.as_secs() != now.as_secs();
        self.clock = now;
        if !tick {
            return false;
        }
        let mut changed = self.live_compaction.is_some();
        for cell in &mut self.cells {
            if cell
                .slice
                .compaction
                .as_ref()
                .is_some_and(|c| c.phase == CompactionPhase::Running)
            {
                cell.cache = None;
                changed = true;
            }
        }
        if changed {
            self.revision = self.revision.saturating_add(1);
        }
        changed
    }
    pub(super) fn compaction(&mut self, index: usize, width: u16, palette: Palette) -> bool {
        let cell = &self.cells[index];
        let Some(info) = &cell.slice.compaction else {
            return false;
        };
        let heading = match info.phase {
            // The first line of the row: "Compaction completed." or, when the
            // row reports both estimates, "Context compacted: X → Y tokens."
            CompactionPhase::Completed => format!(
                "◇ {}",
                cell.slice
                    .text
                    .lines()
                    .next()
                    .unwrap_or("Compaction completed.")
            ),
            CompactionPhase::Running => format!(
                "◇ Compacting… · observed {}s",
                self.clock.saturating_sub(cell.observed_at).as_secs()
            ),
            CompactionPhase::Failed => "◇ Compaction failed.".to_owned(),
        };
        let mut style = palette
            .normal()
            .fg(if info.phase == CompactionPhase::Failed {
                palette.bad
            } else {
                palette.muted
            });
        if self.tool_focus == Some(disclosure::Focus::Compaction(index)) {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        let mut rows = vec![Line::styled(tool_groups::fit(&heading, width), style)];
        if cell.expanded && info.phase != CompactionPhase::Running {
            let detail = if info.phase == CompactionPhase::Failed {
                cell.slice.text.as_str()
            } else {
                cell.slice
                    .text
                    .split_once('\n')
                    .map_or("", |(_, body)| body)
            };
            let mut rendered = markdown::Incremental::default();
            rendered.append(&self.renderer, detail, width.max(1), palette, &mut rows);
            if cell.slice.truncated_bytes > 0 {
                rows.push(Line::styled(
                    format!("[{} source bytes omitted]", cell.slice.truncated_bytes),
                    palette.normal().fg(palette.bad),
                ));
            }
        }
        self.paint_epoch = self.paint_epoch.saturating_add(1);
        let height = rows.len();
        let cell = &mut self.cells[index];
        cell.height = Some((width, height));
        cell.cache = Some(CachedOutput {
            width,
            theme: palette.light,
            row_epochs: vec![self.paint_epoch; height],
            rendered: rows,
            source_end: cell.shown,
            markdown: None,
        });
        self.remember_cache(index);
        true
    }
}
