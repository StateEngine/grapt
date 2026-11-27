// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// Execution-owned elapsed status.
//! Presentation timing is independent of provider token accounting.
use std::time::Duration;
use tokio::time::Instant;

// Text-only Braille frames stay monochrome and one terminal cell wide.
const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// What the seat is doing while it runs.
///
/// Elapsed time alone proves a turn is alive but not what it waits on, so a
/// provider wait, a compaction and ordinary generation all read the same. The
/// phase names the cause from facts the status line already holds.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Generation or tool work, with no phase-specific fact known.
    #[default]
    Thinking,
    /// The session compacts its context before continuing.
    Compacting,
    /// Admitted, but queued behind another run's provider slot.
    WaitingForProvider,
    /// Local preparation capacity is occupied; no provider attempt has opened.
    WaitingForCapacity,
}

#[derive(Default)]
pub struct Activity {
    started: Option<Instant>,
    seconds: u64,
    phase: usize,
    tokens: Option<usize>,
    state: Phase,
}

impl Activity {
    pub fn observe(
        &mut self,
        running: bool,
        tokens: Option<usize>,
        state: Phase,
        now: Instant,
    ) -> bool {
        if !running {
            let changed = self.started.take().is_some() || self.state != Phase::Thinking;
            self.seconds = 0;
            self.phase = 0;
            self.tokens = None;
            self.state = Phase::Thinking;
            return changed;
        }
        let changed = self.started.is_none() || self.tokens != tokens || self.state != state;
        self.started.get_or_insert(now);
        self.tokens = tokens;
        self.state = state;
        self.tick(now) || changed
    }

    pub fn tick(&mut self, now: Instant) -> bool {
        let elapsed = self
            .started
            .map_or(Duration::ZERO, |start| now.saturating_duration_since(start));
        let seconds = elapsed.as_secs();
        let phase = ((elapsed.as_millis() / 100) % FRAMES.len() as u128) as usize;
        let changed = seconds != self.seconds || phase != self.phase;
        self.seconds = seconds;
        self.phase = phase;
        changed
    }

    pub fn active(&self) -> bool {
        self.started.is_some()
    }

    pub fn label(&self) -> Option<String> {
        self.started?;
        // A known phase names its cause; output switches the generic generation
        // phase to visible work without inventing a second cause.
        let state = match self.state {
            Phase::Compacting => "Compacting…",
            Phase::WaitingForProvider => "Waiting for provider slot",
            Phase::WaitingForCapacity => "Waiting for capacity",
            Phase::Thinking if self.tokens.is_some_and(|n| n > 0) => "Working",
            Phase::Thinking => "Thinking…",
        };
        let output = self
            .tokens
            .filter(|n| *n > 0)
            .map_or_else(String::new, |n| {
                let count = grapt_client::format::count(n as u64);
                format!(" · ↓ ~{count} tokens")
            });
        let elapsed = match self.seconds {
            n if n >= 3600 => format!("{}h {}m", n / 3600, n / 60 % 60),
            n if n >= 60 => format!("{}m {}s", n / 60, n % 60),
            n => format!("{n}s"),
        };
        let spinner = FRAMES[self.phase];
        Some(format!("{spinner} {state} ({elapsed}{output})"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quiet_generation_ticks_and_terminal_clears_without_reusing_old_elapsed() {
        let now = Instant::now();
        let mut activity = Activity::default();
        assert!(activity.observe(true, None, Phase::Thinking, now));
        assert!(activity
            .label()
            .is_some_and(|s| s.ends_with("Thinking… (0s)")));
        let initial = activity.label();
        assert!(activity.tick(now + Duration::from_millis(100)));
        assert_ne!(initial, activity.label());
        assert!(activity.tick(now + Duration::from_secs(3)));
        assert!(activity.observe(
            true,
            Some(25),
            Phase::Thinking,
            now + Duration::from_secs(3)
        ));
        assert!(activity
            .label()
            .is_some_and(|s| s.ends_with("Working (3s · ↓ ~25 tokens)")));
        for phase in 0..20 {
            activity.tick(now + Duration::from_millis(3000 + phase * 100));
            assert!(
                activity.label().is_some_and(|s| {
                    matches!(s.chars().next(), Some('\u{2800}'..='\u{28ff}'))
                        && s.split(' ')
                            .next()
                            .is_some_and(|glyph| unicode_width::UnicodeWidthStr::width(glyph) == 1)
                }),
                "spinner must stay within single-cell text-only Braille glyphs"
            );
        }
        assert!(activity.observe(false, None, Phase::Thinking, now + Duration::from_secs(4)));
        assert!(activity.label().is_none());
        activity.observe(true, Some(0), Phase::Thinking, now + Duration::from_secs(9));
        assert!(activity
            .label()
            .is_some_and(|s| s.ends_with("Thinking… (0s)")));
    }

    #[test]
    fn uncounted_output_keeps_only_elapsed_until_a_positive_estimate_arrives() {
        let now = Instant::now();
        let mut activity = Activity::default();
        activity.observe(true, Some(0), Phase::Thinking, now);
        activity.tick(now + Duration::from_secs(131));
        assert!(activity
            .label()
            .is_some_and(|s| s.ends_with("Thinking… (2m 11s)")));
        activity.observe(
            true,
            Some(1),
            Phase::Thinking,
            now + Duration::from_secs(132),
        );
        assert!(activity
            .label()
            .is_some_and(|s| s.ends_with("Working (2m 12s · ↓ ~1 tokens)")));
        activity.observe(true, None, Phase::Thinking, now + Duration::from_secs(133));
        assert!(activity
            .label()
            .is_some_and(|s| s.ends_with("Thinking… (2m 13s)")));
    }

    #[test]
    fn a_known_phase_names_its_cause_and_reverts_when_it_ends() {
        let now = Instant::now();
        let mut activity = Activity::default();
        assert!(activity.observe(true, None, Phase::Compacting, now));
        assert!(activity
            .label()
            .is_some_and(|s| s.ends_with("Compacting… (0s)")));
        activity.tick(now + Duration::from_secs(95));
        assert!(activity
            .label()
            .is_some_and(|s| s.ends_with("Compacting… (1m 35s)")));
        assert!(activity.observe(
            true,
            None,
            Phase::WaitingForProvider,
            now + Duration::from_secs(96)
        ));
        assert!(activity
            .label()
            .is_some_and(|s| s.ends_with("Waiting for provider slot (1m 36s)")));
        assert!(activity.observe(true, None, Phase::Thinking, now + Duration::from_secs(97)));
        assert!(activity
            .label()
            .is_some_and(|s| s.ends_with("Thinking… (1m 37s)")));
    }
}
