//! Coalesce scroll intent once per frame; one page request may be outstanding.
use grapt_client::client::HistoryMove;
use grapt_tui_transcript::transcript::Transcript;

#[derive(Default)]
pub struct Navigation {
    rows: i64,
    superseded: bool,
    pending: Option<(HistoryMove, usize)>,
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

    pub fn obsolete_page(&self, movement: Option<(HistoryMove, bool)>) -> bool {
        self.superseded && movement.is_none_or(|(m, _)| m != HistoryMove::Cancel)
    }

    pub fn clear_scroll(&mut self) {
        self.rows = 0;
        self.superseded = self.pending.is_some();
        if self.superseded {
            self.pending = Some((HistoryMove::Cancel, 0));
        }
    }
    pub fn jump(&mut self, movement: HistoryMove) {
        self.superseded = false;
        self.rows = 0;
        self.pending = Some((movement, 0));
    }

    pub fn failed(&mut self) {
        self.superseded = false;
        self.pending = None;
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
        Some(movement)
    }

    pub fn complete(&mut self, movement: HistoryMove, changed: bool, transcript: &mut Transcript) {
        let Some((expected, remaining)) = self.pending else {
            return;
        };
        if expected != movement {
            return;
        }
        self.pending = None;
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
        assert_eq!(navigation.boundary(&mut transcript), None);
        navigation.scroll(-40);
        assert_eq!(navigation.advance(&mut transcript), None);
        navigation.complete(HistoryMove::Older, true, &mut transcript);
        assert_eq!(transcript.scroll, 11_900);
        navigation.advance(&mut transcript);
        assert_eq!(transcript.scroll, 11_860);
        navigation.jump(HistoryMove::Latest);
        navigation.complete(HistoryMove::Latest, true, &mut transcript);
        assert_eq!(transcript.scroll, 0);
        assert!(!transcript.from_top);
    }
}
