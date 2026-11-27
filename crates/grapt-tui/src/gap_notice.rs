//! A live-event gap is a transient condition, not a standing fault.
//!
//! The client already recovers by re-reading durable history; the notice exists
//! so a user is not silently shown a transcript with a hole in it. Rendering it
//! as a pinned warning block with filler ("Details above; keep editing.") made a
//! self-healing condition look like a failure the user had to act on, and it
//! stayed after recovery had finished.
//!
//! This keeps one neutral line that exists exactly as long as the condition
//! does. It claims nothing about a recovery that has not happened yet, and it
//! does not linger after the fact clears, which would leave the screen asserting
//! something no longer true. It keeps the caveat that matters: live-only details
//! the durable records never carried are not coming back.
const LABEL: &str = "Live view refreshing from durable history; live-only details may be missing.";

#[derive(Default, Clone, PartialEq, Eq)]
pub struct Notice {
    showing: bool,
}

impl Notice {
    /// Observe whether a gap is currently true; report whether that changed.
    pub fn advance(&mut self, gap: bool) -> bool {
        let changed = self.showing != gap;
        self.showing = gap;
        changed
    }

    /// The single neutral line, or `None` once the gap has cleared.
    pub fn label(&self) -> Option<&'static str> {
        self.showing.then_some(LABEL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_neutral_line_shows_only_while_the_gap_is_true() {
        let mut notice = Notice::default();
        assert_eq!(notice.label(), None);
        assert!(notice.advance(true));
        assert_eq!(notice.label(), Some(LABEL));
        // Repeated observations of the same fact change nothing on screen.
        assert!(!notice.advance(true));
        assert_eq!(notice.label(), Some(LABEL));
        assert!(notice.advance(false));
        assert_eq!(notice.label(), None, "the line clears with the condition");
    }
}
