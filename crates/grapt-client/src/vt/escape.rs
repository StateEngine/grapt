//! Bound Escape ambiguity independently of a caller's UI polling interval.
//! Fixed deadline and queued-input precedence.
// Copyright 2025 OpenAI. Modified by Grapt for its bounded byte parser.
use super::{Duration, State, VtEvent, VtParser};

// Console records can split a single logical paste across reader scheduling.
// Keep the measured 50 ms continuation inside the grace, while a real Escape
// remains a single cancel key after at most 100 ms of input silence.
const ESCAPE_GRACE: Duration = Duration::from_millis(100);

impl VtParser {
    /// Feed queued bytes before considering timeout, even after UI work ran late.
    /// `now` is elapsed monotonic time supplied by the sole console owner.
    pub fn feed_at(&mut self, bytes: &[u8], now: Duration) -> Vec<VtEvent> {
        let events = self.feed(bytes);
        if self.state == State::Escape {
            // Nonempty input ending at Escape began a new candidate; an empty
            // poll must never extend a previous candidate's fixed deadline.
            if !bytes.is_empty() || self.escape_since.is_none() {
                self.escape_since = Some(now);
            }
        } else {
            self.escape_since = None;
        }
        events
    }

    /// Poll the byte source with the same fixed ambiguity deadline as the parser.
    /// A zero timeout must check already-ready bytes without waiting.
    pub fn poll_input<E>(
        &mut self,
        timeout: Duration,
        mut elapsed: impl FnMut() -> Duration,
        mut receive: impl FnMut(Duration) -> Result<Option<Vec<u8>>, E>,
    ) -> Result<Vec<VtEvent>, E> {
        let wait = self
            .escape_wait(elapsed())
            .map_or(timeout, |remaining| timeout.min(remaining));
        match receive(wait)? {
            Some(bytes) => Ok(self.feed_at(&bytes, elapsed())),
            None => {
                // A producer may publish between the timed receive and this
                // deadline check. Prefer its queued continuation to Escape.
                if self.escape_wait(elapsed()) == Some(Duration::ZERO) {
                    if let Some(bytes) = receive(Duration::ZERO)? {
                        return Ok(self.feed_at(&bytes, elapsed()));
                    }
                }
                Ok(self.flush_expired_escape(elapsed()).into_iter().collect())
            }
        }
    }

    /// Only a lone Escape needs a timed wake. Idle input, UTF-8 and bracketed
    /// paste continuations remain blocking reads rather than periodic polls.
    pub fn escape_wait(&self, now: Duration) -> Option<Duration> {
        if self.state != State::Escape {
            return None;
        }
        self.escape_since
            .map(|since| ESCAPE_GRACE.saturating_sub(now.saturating_sub(since)))
    }

    /// Call only after the input source reports no queued continuation.
    pub fn flush_expired_escape(&mut self, now: Duration) -> Option<VtEvent> {
        if self.escape_wait(now) != Some(Duration::ZERO) {
            return None;
        }
        self.flush_pending_escape()
    }
}

#[cfg(test)]
mod tests;
