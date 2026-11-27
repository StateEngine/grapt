//! A partial line in a stream is never hidden for long.
//!
//! Streamed text is drawn a line at a time, so appends never move an earlier
//! line boundary. A partial line (a model pausing mid-line, or a long
//! paragraph streaming with no line break at all) would otherwise show
//! nothing until its line ended. Once a partial line has been hidden for
//! [`QUIET`], it is drawn provisionally, whether or not more is still
//! arriving: it seals no boundary, so the line grows in place when the rest
//! arrives, and complete lines still advance line by line. Text that arrives
//! after a reveal waits at most [`QUIET`] for the next one, so redraws stay
//! coalesced, never one per delta.
use super::*;
use std::time::Duration;

/// The longest a partial line stays hidden before it is drawn.
pub(super) const QUIET: Duration = Duration::from_millis(300);

impl Transcript {
    /// Draw the partial line of any open text that has been hidden for
    /// [`QUIET`] at `elapsed`, returning whether anything new is shown.
    pub fn reveal_stalled(&mut self, elapsed: Duration) -> bool {
        let mut changed = false;
        for cell in self.cells.iter_mut().rev() {
            if cell.slice.kind != SliceKind::Text || cell.slice.closed {
                continue;
            }
            let length = cell.slice.text.len();
            let sealed = cell.committed.last().map_or(cell.shown, |line| line.end);
            // Only a lone partial line: complete lines still drain one by one.
            let hidden =
                length > sealed.max(cell.provisional) && !cell.slice.text[sealed..].contains('\n');
            if !hidden {
                cell.hidden_since = None;
                continue;
            }
            match cell.hidden_since {
                Some(since) if elapsed.saturating_sub(since) >= QUIET => {
                    cell.provisional = length;
                    cell.height = None;
                    cell.hidden_since = None;
                    changed = true;
                }
                Some(_) => {}
                None => cell.hidden_since = Some(elapsed),
            }
        }
        if changed {
            self.revision = self.revision.saturating_add(1);
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(text: &str, closed: bool) -> Slice {
        Slice {
            compaction: None,
            kind: SliceKind::Text,
            tool_id: None,
            tool_phase: None,
            ordinal: 0,
            text: text.to_owned(),
            read_detail_start: None,
            tool_summary: None,
            marks: Vec::new(),
            closed,
            footer: None,
            truncated_bytes: 0,
        }
    }

    fn shown(transcript: &mut Transcript, width: u16) -> String {
        transcript
            .layout(0, width, Palette::new(false))
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn streamed(text: &str) -> Transcript {
        let mut transcript = Transcript::new();
        transcript.update(true, 0, vec![open(text, false)]);
        while transcript.draining() {
            transcript.drain();
        }
        transcript
    }

    /// What a finished slice with exactly this text draws.
    fn settled(text: &str, width: u16) -> Vec<Line<'static>> {
        let mut fresh = Transcript::new();
        fresh.update(true, 0, vec![open(text, true)]);
        fresh.layout(0, width, Palette::new(false)).to_vec()
    }

    #[test]
    fn a_partial_line_shows_once_its_stream_goes_quiet() {
        let mut transcript = streamed("First line\nhalf a li");
        assert!(!shown(&mut transcript, 60).contains("half a li"));
        let start = Duration::from_secs(5);
        assert!(!transcript.reveal_stalled(start), "the quiet period starts");
        assert!(!transcript.reveal_stalled(start + QUIET / 2));
        assert!(transcript.reveal_stalled(start + QUIET));
        assert!(shown(&mut transcript, 60).contains("half a li"));
        assert_eq!(
            transcript.layout(0, 60, Palette::new(false)).to_vec(),
            settled("First line\nhalf a li", 60),
            "the partial line draws exactly as that text does"
        );
    }

    #[test]
    fn text_that_keeps_flowing_is_drawn_within_quiet_never_before() {
        // A paragraph with no line break, one delta every 5 ms: the partial
        // line is drawn once it has been hidden for QUIET, then again at most
        // QUIET after the next growth, never once per delta.
        let mut transcript = streamed("First line\nh");
        let mut text = String::from("First line\nh");
        let step = Duration::from_millis(5);
        let mut reveals = Vec::new();
        for tick in 0..200_u32 {
            text.push('a');
            transcript.update(false, 0, vec![open(&text, false)]);
            let now = step * tick;
            if transcript.reveal_stalled(now) {
                reveals.push(now);
                assert!(
                    shown(&mut transcript, 400).contains(&text[11..]),
                    "a reveal draws the whole partial line"
                );
            }
        }
        assert_eq!(reveals.first(), Some(&QUIET), "never before QUIET");
        assert!(
            reveals
                .windows(2)
                .all(|pair| pair[1] - pair[0] <= QUIET + step),
            "flowing text is never hidden for longer than QUIET: {reveals:?}"
        );
        assert_eq!(reveals.len(), 3, "coalesced, not per delta: {reveals:?}");
    }

    #[test]
    fn a_recovered_unbroken_line_draws_within_quiet_while_it_streams() {
        // A re-read hands the transcript a fresh open cell whose whole text is
        // one unbroken line, and the stream keeps flowing.
        let mut text = "long ".repeat(400);
        let mut transcript = streamed(&text);
        let step = Duration::from_millis(5);
        let mut first = None;
        for tick in 0..=60_u32 {
            text.push_str("long ");
            transcript.update(false, 0, vec![open(&text, false)]);
            if transcript.reveal_stalled(step * tick) && first.is_none() {
                first = Some(step * tick);
            }
        }
        assert_eq!(first, Some(QUIET));
    }

    #[test]
    fn the_rest_of_a_revealed_line_grows_it_in_place() {
        for width in [12, 60] {
            let mut transcript = streamed("First line\nhalf a li");
            transcript.reveal_stalled(Duration::ZERO);
            transcript.reveal_stalled(QUIET);
            let full = "First line\nhalf a line, now finished\nnext";
            transcript.update(false, 0, vec![open(full, false)]);
            while transcript.draining() {
                transcript.drain();
            }
            // The sealed line replaces the provisional one, with no duplicate
            // row and no leftover partial; the new partial waits again.
            assert_eq!(
                transcript.layout(0, width, Palette::new(false)).to_vec(),
                settled("First line\nhalf a line, now finished\n", width),
                "width {width}"
            );
        }
    }

    #[test]
    fn complete_lines_still_drain_one_by_one() {
        let mut transcript = Transcript::new();
        transcript.update(true, 0, vec![open("one\ntwo\nthree\npartial", false)]);
        // Lines are queued: the quiet reveal leaves them to the drain.
        assert!(!transcript.reveal_stalled(Duration::ZERO));
        assert!(!transcript.reveal_stalled(QUIET));
        assert!(!shown(&mut transcript, 60).contains("partial"));
    }

    #[test]
    fn wide_characters_reveal_whole() {
        let text = "Done\n中文 👩🏽‍💻 still typing";
        let mut transcript = streamed(text);
        transcript.reveal_stalled(Duration::ZERO);
        assert!(transcript.reveal_stalled(QUIET));
        assert_eq!(
            transcript.layout(0, 9, Palette::new(false)).to_vec(),
            settled(text, 9)
        );
    }
}
