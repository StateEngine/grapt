//! Coalesce scroll intent once per frame; one page request may be outstanding.
use crate::backend::CommandId;
use grapt_client::client::HistoryMove;
use grapt_tui_transcript::transcript::Transcript;

/// A page move that ran, with the command that asked for it and whether the
/// page changed.
pub type Move = (CommandId, HistoryMove, bool);

/// How an update's rows meet the pending page request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// A thumb grab superseded the request: the rows are not shown.
    Obsolete,
    /// Another command moved the page: the rows are shown at the reading
    /// position, which only the pending request's answer may move.
    Foreign,
    Current,
}

#[derive(Default)]
pub struct Navigation {
    rows: i64,
    superseded: bool,
    pending: Option<(HistoryMove, usize)>,
    /// The command whose settlement answers the pending request.
    ticket: Option<CommandId>,
    pub edges: (bool, bool),
}

impl Navigation {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn loading(&self) -> bool {
        self.pending.is_some()
    }
    pub fn scroll(&mut self, rows: i64) {
        self.rows = self.rows.saturating_add(rows).clamp(-1_000_000, 1_000_000);
    }

    pub fn preserves_reading(&self) -> bool {
        self.superseded
    }

    /// How an update's rows meet the pending request, given the page moves
    /// it carries.
    pub fn page(&self, moves: &[Move]) -> Page {
        let answer = moves.iter().find(|(id, ..)| self.ticket == Some(*id));
        if self.superseded && answer.is_none_or(|(_, movement, _)| *movement != HistoryMove::Cancel)
        {
            Page::Obsolete
        } else if self.ticket.is_some() && !moves.is_empty() && answer.is_none() {
            Page::Foreign
        } else {
            Page::Current
        }
    }

    pub fn clear_scroll(&mut self) {
        self.rows = 0;
        self.superseded = self.pending.is_some();
        if self.superseded {
            self.pending = Some((HistoryMove::Cancel, 0));
            self.ticket = None;
        }
    }
    pub fn jump(&mut self, movement: HistoryMove) {
        self.superseded = false;
        self.rows = 0;
        self.pending = Some((movement, 0));
        self.ticket = None;
    }

    pub fn failed(&mut self) {
        self.superseded = false;
        self.pending = None;
        self.ticket = None;
    }

    /// A jump to `movement` was sent as command `id`.
    pub fn jumped(&mut self, movement: HistoryMove, id: CommandId) {
        self.jump(movement);
        self.sent(id);
    }

    /// The pending request was sent as command `id`.
    pub fn sent(&mut self, id: CommandId) {
        if self.pending.is_some() {
            self.ticket = Some(id);
        }
    }

    /// Commands a quit discarded: the pending request settles, leaving the
    /// view where it is, only when it is one of them.
    pub fn dropped(&mut self, discarded: &[CommandId]) {
        if self
            .ticket
            .is_some_and(|ticket| discarded.contains(&ticket))
        {
            self.failed();
        }
    }

    pub fn advance(&mut self, transcript: &mut Transcript) -> Option<HistoryMove> {
        if transcript.dragging_scrollbar() {
            return None;
        }
        if self.pending.is_some() {
            return None;
        }
        let rows = std::mem::take(&mut self.rows);
        let amount = rows.unsigned_abs() as usize;
        let older = rows > 0;
        if amount == 0 {
            return None;
        }
        if let Some(remaining) = transcript.scroll_seek(older, amount) {
            return if remaining > 0 {
                self.request(older, remaining)
            } else {
                None
            };
        }
        if older == transcript.from_top {
            let remainder = amount.saturating_sub(transcript.scroll);
            transcript.scroll = transcript.scroll.saturating_sub(amount);
            if remainder > 0 {
                return self.request(older, remainder);
            }
        } else {
            transcript.scroll = transcript.scroll.saturating_add(amount);
        }
        None
    }

    pub fn boundary(&mut self, transcript: &mut Transcript) -> Option<HistoryMove> {
        if transcript.dragging_scrollbar() {
            return None;
        }
        let (older, remaining) = transcript.overflow.take()?;
        self.request(older, remaining)
    }

    fn request(&mut self, older: bool, remaining: usize) -> Option<HistoryMove> {
        if self.pending.is_some() || !(if older { self.edges.0 } else { self.edges.1 }) {
            return None;
        }
        let movement = if older {
            HistoryMove::Older
        } else {
            HistoryMove::Newer
        };
        self.pending = Some((movement, remaining));
        self.ticket = None;
        Some(movement)
    }

    /// Settle the pending request when `id` is the command that sent it.
    pub fn complete(&mut self, (id, movement, changed): Move, transcript: &mut Transcript) {
        let Some((expected, remaining)) = self.pending else {
            return;
        };
        if expected != movement || self.ticket != Some(id) {
            return;
        }
        self.pending = None;
        self.ticket = None;
        // A thumb grab owns a newer reading choice than this page request.
        if std::mem::take(&mut self.superseded) {
            return;
        }
        transcript.from_top = if changed {
            matches!(movement, HistoryMove::Beginning | HistoryMove::Newer)
        } else {
            matches!(movement, HistoryMove::Beginning | HistoryMove::Older)
        };
        transcript.scroll = if changed { remaining } else { 0 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_hundred_scroll_keys_become_one_jump_and_one_page_request() {
        let handoff = crate::backend::Handoff::default();
        let id = || handoff.envelope(crate::backend::Command::Models).0;
        let mut navigation = Navigation {
            edges: (true, true),
            ..Navigation::default()
        };
        let mut transcript = Transcript::new();
        for _ in 0..600 {
            navigation.scroll(20);
        }
        assert_eq!(
            transcript.scroll, 0,
            "keys do not move or lay out rows individually"
        );
        assert_eq!(navigation.advance(&mut transcript), None);
        assert_eq!(transcript.scroll, 12_000);
        transcript.overflow = Some((true, 11_900));
        assert_eq!(
            navigation.boundary(&mut transcript),
            Some(HistoryMove::Older)
        );
        let older = id();
        navigation.sent(older);
        assert_eq!(navigation.boundary(&mut transcript), None);
        navigation.scroll(-40);
        assert_eq!(navigation.advance(&mut transcript), None);
        navigation.complete((older, HistoryMove::Older, true), &mut transcript);
        assert_eq!(transcript.scroll, 11_900);
        navigation.advance(&mut transcript);
        assert_eq!(transcript.scroll, 11_860);
        navigation.jump(HistoryMove::Latest);
        let latest = id();
        navigation.sent(latest);
        navigation.complete((latest, HistoryMove::Latest, true), &mut transcript);
        assert_eq!(transcript.scroll, 0);
        assert!(!transcript.from_top);
    }
}
