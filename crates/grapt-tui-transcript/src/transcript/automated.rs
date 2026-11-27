//! Automated packets stay in the transcript, outside the human editor and recall.
use super::{CachedOutput, Transcript};
use crate::palette::Palette;
use grapt_client::SliceKind;

impl Transcript {
    pub(super) fn remember_cache(&mut self, index: usize) {
        self.cached.retain(|old| *old != index);
        self.cached.push_back(index);
        while self.cached.len() > 128 {
            if let Some(old) = self.cached.pop_front() {
                self.cells[old].cache = None;
            }
        }
    }

    pub fn latest_is_automated(&self) -> bool {
        self.cells
            .iter()
            .rev()
            .find(|cell| {
                matches!(
                    cell.slice.kind,
                    SliceKind::User | SliceKind::Automated | SliceKind::SeatStartup
                )
            })
            .is_some_and(|cell| {
                matches!(
                    cell.slice.kind,
                    SliceKind::Automated | SliceKind::SeatStartup
                )
            })
    }

    /// Toggle the newest visible automated input, without editing the draft.
    pub fn toggle_details(&mut self) {
        let index = self
            .window
            .iter()
            .map(|slot| slot.index)
            .filter(|&index| {
                matches!(
                    self.cells[index].slice.kind,
                    SliceKind::Automated | SliceKind::SeatStartup
                )
            })
            .max();
        let Some(index) = index else {
            return;
        };
        let cell = &mut self.cells[index];
        cell.expanded = !cell.expanded;
        cell.cache = None;
        cell.height = None;
        self.revision = self.revision.saturating_add(1);
    }

    pub(super) fn collapsed_automated(
        &mut self,
        index: usize,
        width: u16,
        palette: Palette,
    ) -> bool {
        let cell = &mut self.cells[index];
        if !matches!(
            cell.slice.kind,
            SliceKind::Automated | SliceKind::SeatStartup
        ) || cell.expanded
        {
            return false;
        }
        self.paint_epoch = self.paint_epoch.saturating_add(1);
        cell.cache = Some(CachedOutput {
            width,
            theme: palette.light,
            rendered: vec![super::input_blocks::notice(
                if cell.slice.kind == SliceKind::SeatStartup {
                    "Seat startup prompt · F8 expand"
                } else {
                    "Automated prompt · F8 expand"
                },
                width,
                palette,
            )],
            row_epochs: vec![self.paint_epoch],
            source_end: cell.shown,
            markdown: None,
        });
        cell.height = Some((width, 1));
        self.remember_cache(index);
        true
    }
}
