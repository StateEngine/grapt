//! Live frames wait behind a session-catalog refresh, so a session the
//! catalog has not listed yet never acquires observations or a foreign view.
//! A catalog that cannot be read gives way to a resync instead: the event
//! reader blocks on a full handoff, so waiting forever would hide a dead
//! stream. A slow refresh that succeeds never drops a frame.
use grapt_client::FramePoll;
use std::time::Duration;
use tokio::time::Instant;

/// Consecutive failed refreshes after which a held frame gives way.
const FAILURES: u32 = 3;
/// A backstop for a refresh that neither succeeds nor fails.
const BACKSTOP: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub(crate) struct CatalogGate {
    held: Option<FramePoll>,
    since: Option<Instant>,
    backstop: Duration,
    failures: u32,
}

/// What the stream does with the next frame.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Gate {
    /// Deliver this frame, or nothing when none was waiting.
    Pass(Option<FramePoll>),
    /// The catalog is being refreshed; the frame waits.
    Hold,
    /// The catalog cannot be read: drop the held frame and resynchronise.
    Resync,
}

impl Default for CatalogGate {
    fn default() -> Self {
        Self::with_backstop(BACKSTOP)
    }
}

impl CatalogGate {
    pub(crate) fn with_backstop(backstop: Duration) -> Self {
        Self {
            held: None,
            since: None,
            backstop,
            failures: 0,
        }
    }

    /// The frame held from an earlier pass, which goes first.
    pub(crate) fn take(&mut self) -> Option<FramePoll> {
        self.held.take()
    }

    /// Holds `frame` while the catalog is `dirty`, or lets it through.
    pub(crate) fn admit(&mut self, frame: Option<FramePoll>, dirty: bool) -> Gate {
        if !dirty {
            self.since = None;
            return Gate::Pass(frame);
        }
        if frame.is_some() {
            let since = *self.since.get_or_insert_with(Instant::now);
            if self.failures >= FAILURES || since.elapsed() >= self.backstop {
                self.reset();
                self.failures = 0;
                return Gate::Resync;
            }
        }
        self.held = frame;
        Gate::Hold
    }

    /// The backend reports each catalog refresh it attempts.
    pub(crate) fn refreshed(&mut self, succeeded: bool) {
        self.failures = if succeeded {
            0
        } else {
            self.failures.saturating_add(1)
        };
    }

    /// A gap or a new generation: nothing held carries over, and the next
    /// hold starts its own clock.
    pub(crate) fn reset(&mut self) {
        self.held = None;
        self.since = None;
    }

    #[cfg(test)]
    pub(crate) fn hold(&mut self, frame: FramePoll) {
        self.held = Some(frame);
    }

    #[cfg(test)]
    pub(crate) fn holding_since(&self) -> Option<Instant> {
        self.since
    }
}

#[cfg(test)]
mod tests;
