//! Only canonical Client form reads/writes; no inferred settlement or automatic replay.
use grapt_client::{Client, ClientError, Connection, PeriodicRead};
mod transaction;
use std::time::Duration;
use tokio::time::Instant;
pub(super) use transaction::{begin, Flow};
const INTERVAL: Duration = Duration::from_secs(2);

/// Each form read's due time, and a hint that arrived while that read was
/// in flight: no past-due deadline is kept while a read runs.
#[derive(Debug)]
pub struct Poll {
    session_due: Option<Instant>,
    location_due: Option<Instant>,
    session_hint: bool,
    location_hint: bool,
}
impl Poll {
    pub fn new(now: Instant) -> Self {
        Self {
            session_due: Some(now),
            location_due: Some(now),
            session_hint: false,
            location_hint: false,
        }
    }
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        self.session_due
            .into_iter()
            .chain(self.location_due)
            .min()
            .map(|due| due.saturating_duration_since(now))
    }
    /// The form reads due now, each marked started. A read already running
    /// stays due and starts once it settles.
    pub fn due<C: Connection>(
        &mut self,
        client: &mut Client<C>,
        now: Instant,
        busy: impl Fn(PeriodicRead) -> bool,
    ) -> Vec<PeriodicRead> {
        // A pending resume starts no read; due and flagged reads stay queued.
        if client.resume_pending() {
            return Vec::new();
        }
        let hints = [
            client.take_session_form_refresh(),
            client.take_location_form_refresh(),
        ];
        let mut due = Vec::new();
        for ((read, at, held), hinted) in [
            (
                PeriodicRead::SessionForms,
                &mut self.session_due,
                &mut self.session_hint,
            ),
            (
                PeriodicRead::LocationForms,
                &mut self.location_due,
                &mut self.location_hint,
            ),
        ]
        .into_iter()
        .zip(hints)
        {
            if hinted {
                *at = Some(now);
            }
            if !at.is_some_and(|at| now >= at) {
                continue;
            }
            *at = None;
            if busy(read) {
                // Held until the running read settles, never as a deadline.
                *held = true;
            } else {
                due.push(read);
            }
        }
        due
    }

    /// Record a form read's outcome. Committed events, attachment and recovery
    /// trigger canonical reads; only a failed read schedules a retry, and an
    /// answer for a Session or location no longer current asks again at once.
    pub fn settled(
        &mut self,
        read: PeriodicRead,
        result: Result<bool, ClientError>,
        now: Instant,
    ) -> Result<bool, ClientError> {
        let (at, held) = match read {
            PeriodicRead::SessionForms => (&mut self.session_due, &mut self.session_hint),
            _ => (&mut self.location_due, &mut self.location_hint),
        };
        let hinted = std::mem::take(held);
        match result {
            Ok(true) => {
                *at = hinted.then_some(now);
                Ok(true)
            }
            Ok(false) => {
                *at = Some(now);
                Ok(false)
            }
            Err(error) => {
                *at = Some(now + INTERVAL);
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests;
