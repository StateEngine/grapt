//! Monotonic allowances for fixture progress, independent of poll count.
use std::{thread, time::Duration};
use tokio::time::Instant;

pub(super) struct PollDeadline {
    started: Instant,
    allowance: Duration,
}

impl PollDeadline {
    pub(super) fn new() -> Self {
        Self::within(Duration::from_secs(60))
    }

    pub(super) fn within(allowance: Duration) -> Self {
        Self {
            started: Instant::now(),
            allowance,
        }
    }

    pub(super) fn pending(&self) -> bool {
        self.started.elapsed() < self.allowance
    }

    pub(super) fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub(super) fn poll<T>(
        &self,
        interval: Duration,
        mut observe: impl FnMut() -> Option<T>,
    ) -> Option<T> {
        loop {
            if let Some(value) = observe() {
                return Some(value);
            }
            if !self.pending() {
                return None;
            }
            thread::sleep(interval);
        }
    }
}

impl std::fmt::Display for PollDeadline {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{:?} elapsed (allowance {:?})",
            self.elapsed(),
            self.allowance
        )
    }
}
