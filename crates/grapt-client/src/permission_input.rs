//! Shared post-display input window for permission decisions in both frontends.
use std::time::Duration;

const ARM_WINDOW: Duration = Duration::from_millis(400);

/// A request starts unarmed; presentation and input use the same monotonic clock.
#[derive(Debug, Default)]
pub struct PermissionInput {
    visible_since: Option<Duration>,
}

impl PermissionInput {
    /// Record the first successful flush, not every subsequent repaint.
    pub fn presented(&mut self, now: Duration) {
        self.visible_since.get_or_insert(now);
    }

    /// Only input after the full visible window may answer this request.
    pub fn ready(&self, now: Duration) -> bool {
        self.visible_since
            .is_some_and(|since| now.saturating_sub(since) >= ARM_WINDOW)
    }
}
