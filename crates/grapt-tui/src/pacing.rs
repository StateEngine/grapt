//! Coalesce stream frames; input may interrupt the pending deadline.
use std::time::Duration;
use tokio::time::Instant;

pub const INTERVAL: Duration = Duration::from_nanos(16_666_667);

pub fn due(elapsed: Duration, input: bool) -> bool {
    input || elapsed >= INTERVAL
}

/// Give input a short clear window and coalesce stream frames during recent
/// typing. This prevents closely spaced stream writes from delaying console echo.
/// Only pending work consults the deadline; input paints bypass it.
/// It does not schedule a timer or delay the existing immediate input paint.
pub struct TypingPause {
    enabled: bool,
    until: Option<Instant>,
    coalesce_stream: bool,
    recent_input: Option<Instant>,
    last_paint: Option<Instant>,
}

impl TypingPause {
    pub fn new(setting: Option<&str>) -> Self {
        Self {
            enabled: setting.is_none_or(|value| value == "50"),
            until: None,
            // Explicit 50 keeps the original policy for matched comparisons.
            coalesce_stream: setting.is_none(),
            recent_input: None,
            last_paint: None,
        }
    }
    pub fn edited(&mut self, now: Instant) {
        if self.enabled {
            self.until = Some(now + Duration::from_millis(50));
            self.recent_input = Some(now + Duration::from_millis(500));
        }
    }
    pub fn cancel(&mut self) {
        self.until = None;
        self.recent_input = None;
    }
    pub fn painted(&mut self, now: Instant) {
        self.last_paint = Some(now);
    }
    pub fn remaining(&self, now: Instant) -> Duration {
        let quiet = self
            .until
            .map(|until| until.saturating_duration_since(now))
            .unwrap_or_default();
        let stream = match (self.recent_input, self.last_paint) {
            (Some(recent), Some(painted)) if self.coalesce_stream && recent > now => (painted
                + Duration::from_millis(100))
            .min(recent)
            .saturating_duration_since(now),
            _ => Duration::ZERO,
        };
        quiet.max(stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_typing_coalesces_stream_frames_without_restarting_their_deadline() {
        let start = Instant::now();
        let mut pause = TypingPause::new(None);
        pause.painted(start);
        assert!(pause.remaining(start).is_zero());
        pause.edited(start + Duration::from_millis(10));
        assert_eq!(
            pause.remaining(start + Duration::from_millis(50)),
            Duration::from_millis(50)
        );
        pause.edited(start + Duration::from_millis(20));
        assert!(pause
            .remaining(start + Duration::from_millis(100))
            .is_zero());
        pause.painted(start + Duration::from_millis(100));
        assert_eq!(
            pause.remaining(start + Duration::from_millis(150)),
            Duration::from_millis(50)
        );
        pause.painted(start + Duration::from_millis(500));
        assert_eq!(
            pause.remaining(start + Duration::from_millis(510)),
            Duration::from_millis(10)
        );
        assert!(pause
            .remaining(start + Duration::from_millis(520))
            .is_zero());
        pause.edited(start + Duration::from_millis(600));
        pause.painted(start + Duration::from_millis(600));
        pause.cancel();
        assert!(pause
            .remaining(start + Duration::from_millis(601))
            .is_zero());
        // The retained comparison flag isolates the old 50 ms policy.
        let mut comparison = TypingPause::new(Some("50"));
        comparison.painted(start);
        comparison.edited(start);
        assert!(comparison
            .remaining(start + Duration::from_millis(50))
            .is_zero());
    }

    #[test]
    fn typing_defers_only_stream_work_and_edits_restart_the_bounded_deadline() {
        let start = Instant::now();
        let mut pause = TypingPause::new(Some("50"));
        assert!(pause.remaining(start).is_zero());
        pause.edited(start);
        assert_eq!(pause.remaining(start), Duration::from_millis(50));
        pause.edited(start + Duration::from_millis(20));
        assert_eq!(
            pause.remaining(start + Duration::from_millis(50)),
            Duration::from_millis(20)
        );
        assert!(pause.remaining(start + Duration::from_millis(70)).is_zero());
        pause.edited(start + Duration::from_millis(80));
        pause.cancel();
        assert!(pause.remaining(start + Duration::from_millis(81)).is_zero());
        let mut disabled = TypingPause::new(Some("0"));
        disabled.edited(start);
        assert!(disabled.remaining(start).is_zero());
    }

    #[test]
    fn typing_pause_is_on_by_default_and_retains_the_comparison_switch() {
        let now = Instant::now();
        for (setting, milliseconds) in [
            (None, 50),
            (Some("50"), 50),
            (Some("0"), 0),
            (Some("other"), 0),
        ] {
            let mut pause = TypingPause::new(setting);
            pause.edited(now);
            assert_eq!(pause.remaining(now), Duration::from_millis(milliseconds));
        }
    }

    #[test]
    fn recorded_stream_events_coalesce_at_sixty_fps_and_input_preempts() {
        let mut last = Duration::ZERO;
        let mut frames = 0;
        for millisecond in 1..=1_000 {
            let now = Duration::from_millis(millisecond);
            if due(now - last, false) {
                frames += 1;
                last = now;
            }
        }
        assert!(
            (58..=60).contains(&frames),
            "one-second frame count: {frames}"
        );
        let input_times = [3, 7, 11, 100, 199];
        let mut last = Duration::ZERO;
        let mut input_frames = Vec::new();
        for millisecond in 1..=200 {
            let now = Duration::from_millis(millisecond);
            let input = input_times.contains(&millisecond);
            let elapsed = now - last;
            if due(elapsed, input) {
                if input {
                    input_frames.push(millisecond);
                } else {
                    assert!(elapsed >= INTERVAL);
                }
                last = now;
            }
        }
        assert_eq!(input_frames, input_times);
    }
}
