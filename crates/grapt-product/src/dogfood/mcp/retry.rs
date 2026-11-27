//! Local startup backoff. The registry remains the only connection owner.
//!
//! Bounded delay/deadline mechanics.
//! Copyright OpenAI. Licensed under the Apache License, Version 2.0.

use std::time::Duration;

/// Maximum active time for a recovery round; startup human scopes are excluded
/// and separately share a finite ten-minute allowance.
pub(super) const ROUND_BUDGET: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy)]
pub(super) struct Backoff {
    failures: u8,
    failed_at: i64,
}

impl Backoff {
    pub(super) fn failed(previous: Option<Self>, now: i64) -> Self {
        let failures = previous.map_or(1, |state| state.failures.saturating_add(1));
        Self {
            failures,
            failed_at: now,
        }
    }

    pub(super) fn remaining(self, now: i64) -> Duration {
        let seconds = match self.failures {
            1 => 1,
            2 => 2,
            3 => 4,
            4 => 8,
            5 => 16,
            _ => 30,
        };
        // Relative elapsed time avoids overflowing a deadline near i64::MAX;
        // rollback cannot wait longer than the original capped delay.
        let elapsed = now.saturating_sub(self.failed_at).max(0);
        Duration::from_secs(seconds).saturating_sub(Duration::from_millis(
            u64::try_from(elapsed).unwrap_or(u64::MAX),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_retries_double_then_cap_and_expire() {
        let mut previous = None;
        for seconds in [1, 2, 4, 8, 16, 30, 30, 30] {
            let state = Backoff::failed(previous, 100_000);
            assert_eq!(state.remaining(100_000), Duration::from_secs(seconds));
            assert_eq!(state.remaining(200_000), Duration::ZERO);
            assert_eq!(state.remaining(i64::MIN), Duration::from_secs(seconds));
            previous = Some(state);
        }
        assert_eq!(
            Backoff::failed(None, 0).remaining(0),
            Duration::from_secs(1)
        );
        assert_eq!(
            Backoff::failed(None, i64::MAX).remaining(i64::MAX),
            Duration::from_secs(1)
        );
        assert_eq!(
            Backoff::failed(None, i64::MIN).remaining(i64::MAX),
            Duration::ZERO
        );
    }
}
