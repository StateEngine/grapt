//! Splicing an older history window in front of the one a view holds.
//!
//! A reopened session folds its NEWEST page first and goes live on it; older
//! pages arrive afterwards and are folded on their own, then placed in front.
//! The fold is the same `apply_durable` either way, so the retained slices are
//! the ones a single oldest-first fold of the same window would have kept.

use super::*;

impl SessionView {
    /// Place `older`, folded from history strictly before this view's, in
    /// front of it.
    ///
    /// Everything that describes the PRESENT stays this view's: the execution
    /// state, the held terminal, live quiescence and any open live slice. The
    /// older window contributes slices, counts and accounting only, so a
    /// backfill can never create a terminal or settle a turn.
    pub(crate) fn prepend(&mut self, mut older: SessionView, bounds: Bounds) {
        // Ordinals stay unique and increasing in reading order.
        let shift = older.next_ordinal;
        // Nothing older than this view is still streaming: an answer it left
        // unfinished was interrupted or lost, and takes no live text.
        for slice in &mut older.slices {
            if matches!(slice.kind, SliceKind::Text | SliceKind::Reasoning) {
                slice.closed = true;
            }
        }
        for slice in &mut self.slices {
            slice.ordinal = slice.ordinal.saturating_add(shift);
        }
        // Identities move with the ordinals they key; the older window's
        // slices are closed above, so none of its identities stays claimable.
        self.slice_wires = std::mem::take(&mut self.slice_wires)
            .into_iter()
            .map(|(ordinal, wire)| (ordinal.saturating_add(shift), wire))
            .collect();
        self.next_ordinal = self.next_ordinal.saturating_add(shift);
        self.older_len = self.older_len.saturating_add(older.slices.len());
        older.slices.append(&mut self.slices);
        self.slices = older.slices;
        // Retention evicts the OLDEST, as it would have in one fold, and the
        // eviction is counted rather than silent.
        let excess = self.slices.len().saturating_sub(bounds.max_retained_slices);
        if excess > 0 {
            self.slices.drain(..excess);
            self.dropped_slices = self.dropped_slices.saturating_add(excess);
            self.evicted_front(excess);
            let held = self
                .slices
                .first()
                .map_or(usize::MAX, |slice| slice.ordinal);
            self.slice_wires.retain(|ordinal, _| *ordinal >= held);
        }
        self.dropped_slices = self.dropped_slices.saturating_add(older.dropped_slices);
        self.folded_user_rows = self.folded_user_rows.saturating_add(older.folded_user_rows);
        // The newest durable accounting is the last record that carried it:
        // this window's when it has one, otherwise the older window's.
        if self.usage == Usage::default() {
            self.usage = older.usage;
        }
        self.cache.prepend(older.cache);
    }

    /// Account for `count` slices evicted from the front: older history goes
    /// first, and once the tail itself is cut its row map no longer holds.
    pub(super) fn evicted_front(&mut self, count: usize) {
        if count > self.older_len {
            self.tail_rows.clear();
        }
        self.older_len = self.older_len.saturating_sub(count);
    }

    /// This view's scrollback strictly OLDER than a newer tail whose rows,
    /// oldest first, are `tail`: everything before the slices of the newer
    /// tail's oldest row, which this view's own tail must still name.
    ///
    /// `None` when that cannot be shown — the row is not in this view's tail
    /// (more rows arrived than one page holds), or the tail was cut — because
    /// carrying slices across an unproven seam could duplicate or drop rows.
    pub(crate) fn older_than(
        &self,
        tail: &[(crate::wire::MessageWireId, usize)],
    ) -> Option<SessionView> {
        let (oldest, _) = tail.first()?;
        let seam = self.tail_rows.iter().position(|(id, _)| id == oldest)?;
        let len = self
            .tail_rows
            .iter()
            .take(seam)
            .fold(self.older_len, |len, (_, slices)| {
                len.saturating_add(*slices)
            });
        let slices = self.slices.get(..len)?.to_vec();
        let mut carried = SessionView::new(&self.session_id);
        carried.next_ordinal = slices
            .last()
            .map_or(0, |slice| slice.ordinal.saturating_add(1));
        carried.slices = slices;
        Some(carried)
    }

    /// Replace the older history in front of the tail with a freshly read
    /// window, in one step: the scrollback a re-read carried stays on screen
    /// until the walk that replaces it has finished.
    pub(crate) fn replace_older(&mut self, fresh: SessionView, bounds: Bounds) {
        self.slices.drain(..self.older_len.min(self.slices.len()));
        let held = self
            .slices
            .first()
            .map_or(usize::MAX, |slice| slice.ordinal);
        self.slice_wires.retain(|ordinal, _| *ordinal >= held);
        self.older_len = 0;
        self.prepend(fresh, bounds);
    }
}
