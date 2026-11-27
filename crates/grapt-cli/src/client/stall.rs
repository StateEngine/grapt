//! The transport-stall policy for the durable re-reads.
//!
//! A service that stops answering must not end the run at the first timed
//! out request: the run's permission, activity and message polls are
//! re-reads of durable state, so a transport failure reissues the request.
//! The patience is bounded in pure `Duration` arithmetic by a
//! consecutive-failure count and by the total time a streak may consume:
//! what each failure actually awaited — a timeout waits out the whole
//! request allowance, every other failure waits its measured monotonic
//! time, capped at the allowance — plus every backoff sleep. The measured
//! wait arrives from the caller on the runtime's pausable monotonic clock,
//! so this crate stays banned from ambient clocks, and a connect-kind
//! failure that consumed real time (a TLS handshake held before the close)
//! charges that time instead of pretending it failed instantly. Never a
//! wall-clock window.

use std::time::Duration;

use super::REQUEST_ALLOWANCE;

/// Consecutive transport failures a durable re-read tolerates before the
/// run ends: five at the 30 s request allowance, so a stalled service
/// cannot hold a run longer than `run::TRANSPORT_STALL_EXIT_BOUND`.
pub const MAX_CONSECUTIVE_TRANSPORT_FAILURES: u32 = 5;
/// The total request time plus backoff a streak may consume before it ends
/// the run. A timeout or a late body failure may have waited out the whole
/// request allowance, so each is charged the full allowance; a connect-kind
/// failure charges only what it actually awaited, plus its backoff.
pub const MAX_TRANSPORT_STALL_TIME: Duration =
    REQUEST_ALLOWANCE.saturating_mul(MAX_CONSECUTIVE_TRANSPORT_FAILURES);

/// Consecutive transport failures on one endpoint, shared by the durable
/// re-reads that poll it. A successful re-read empties the streak.
#[derive(Debug, Default)]
pub(crate) struct TransportStreak {
    failures: u32,
    time_spent: Duration,
    diagnostic_printed: bool,
    last_error: Option<String>,
}

impl TransportStreak {
    /// A reissue is allowed only while the full request allowance it may
    /// still need fits inside the total time budget. Checked before every
    /// attempt, so a budget-crossing attempt is never started.
    pub(crate) fn may_reissue(&self) -> bool {
        self.time_spent
            .checked_add(REQUEST_ALLOWANCE)
            .is_some_and(|total| total <= MAX_TRANSPORT_STALL_TIME)
    }

    /// Records one transport failure. `waited` is the time the attempt
    /// actually consumed, measured on the runtime's monotonic clock: a
    /// timeout waited out the whole request allowance, and every other
    /// failure charges what it waited, capped at the allowance. `is_connect`
    /// names a connect-kind failure — it may be the service dying under the
    /// embedded product, and it buys the budget-clipped exponential backoff
    /// (1 s doubling to 8 s). A timeout or a late body failure buys no
    /// backoff. `error` is the transport's own description, kept for the
    /// stalled report. Returns the backoff to sleep before the reissue, and
    /// whether the streak has now stalled the client.
    pub(crate) fn record_failure(
        &mut self,
        waited: Duration,
        is_timeout: bool,
        is_connect: bool,
        error: String,
    ) -> (Duration, bool) {
        let request_time = if is_timeout {
            REQUEST_ALLOWANCE
        } else {
            waited.min(REQUEST_ALLOWANCE)
        };
        let headroom = MAX_TRANSPORT_STALL_TIME
            .saturating_sub(self.time_spent)
            .saturating_sub(request_time);
        let backoff = if is_connect && !is_timeout {
            Duration::from_secs(1u64 << self.failures.min(3)).min(headroom)
        } else {
            Duration::ZERO
        };
        self.failures = self.failures.saturating_add(1);
        self.time_spent = self
            .time_spent
            .saturating_add(request_time)
            .saturating_add(backoff);
        self.last_error = Some(error);
        let stalled = self.failures >= MAX_CONSECUTIVE_TRANSPORT_FAILURES
            || self.time_spent > MAX_TRANSPORT_STALL_TIME;
        (backoff, stalled)
    }

    /// A durable re-read completed: the streak is empty again.
    pub(crate) fn reset(&mut self) {
        self.failures = 0;
        self.time_spent = Duration::ZERO;
        self.diagnostic_printed = false;
        self.last_error = None;
    }

    pub(crate) fn diagnostic_due(&mut self) -> bool {
        if self.diagnostic_printed {
            return false;
        }
        self.diagnostic_printed = true;
        true
    }

    pub(crate) const fn failures(&self) -> u32 {
        self.failures
    }

    /// The last transport error the streak recorded, for the stalled report.
    pub(crate) fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// The request time plus backoff the streak has consumed so far.
    #[cfg(test)]
    pub(crate) const fn time_spent(&self) -> Duration {
        self.time_spent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The measured review repro shape: a TLS handshake held 1.6 s before
    /// the close. It is a connect-kind failure that consumed real time.
    const HELD_HANDSHAKE_WAIT: Duration = Duration::from_millis(1600);

    #[test]
    fn a_streak_of_timeouts_stalls_at_the_failure_count() {
        let mut streak = TransportStreak::default();
        let mut stalled = false;
        for attempt in 1..=MAX_CONSECUTIVE_TRANSPORT_FAILURES {
            let (backoff, is_stalled) =
                streak.record_failure(REQUEST_ALLOWANCE, true, false, "timed out".to_owned());
            assert_eq!(
                backoff,
                Duration::ZERO,
                "a timeout waited out its allowance, so it buys no backoff"
            );
            stalled = is_stalled;
            assert!(
                !stalled || attempt == MAX_CONSECUTIVE_TRANSPORT_FAILURES,
                "the count bound fires on exactly the {attempt}th failure"
            );
        }
        assert!(stalled, "the streak stalled at the failure count");
        assert_eq!(streak.failures(), MAX_CONSECUTIVE_TRANSPORT_FAILURES);
        assert_eq!(
            streak.time_spent(),
            MAX_TRANSPORT_STALL_TIME,
            "each timed out re-read charged the full request allowance"
        );
    }

    #[test]
    fn a_streak_of_instant_refusals_backs_off_and_stays_within_the_total_time_bound() {
        let mut streak = TransportStreak::default();
        let expected = [1_u64, 2, 4, 8, 8];
        for (position, seconds) in expected.iter().enumerate() {
            let (backoff, is_stalled) =
                streak.record_failure(Duration::ZERO, false, true, "refused".to_owned());
            assert_eq!(
                backoff,
                Duration::from_secs(*seconds),
                "failure {position} backs off {backoff:?}"
            );
            assert!(
                !is_stalled || position + 1 == expected.len(),
                "the count bound fires on exactly the {position}th failure"
            );
        }
        assert_eq!(streak.failures(), MAX_CONSECUTIVE_TRANSPORT_FAILURES);
        assert!(
            streak.time_spent() <= MAX_TRANSPORT_STALL_TIME,
            "an instant refusal charges only its measured wait plus backoff: {:?} vs bound {MAX_TRANSPORT_STALL_TIME:?}",
            streak.time_spent()
        );
    }

    #[test]
    fn a_delayed_connect_failure_charges_its_measured_wait() {
        // The exact review repro: a TLS handshake held 1.6 s before the
        // close is connect-kind, and it consumed real time. The old
        // policy charged it zero request time; the measured policy
        // charges what it waited, plus its backoff.
        let mut streak = TransportStreak::default();
        let (backoff, stalled) = streak.record_failure(
            HELD_HANDSHAKE_WAIT,
            false,
            true,
            "handshake held".to_owned(),
        );
        assert!(!stalled, "one held handshake has not stalled the client");
        assert_eq!(backoff, Duration::from_secs(1));
        assert_eq!(
            streak.time_spent(),
            HELD_HANDSHAKE_WAIT + Duration::from_secs(1),
            "the measured wait is charged, never zero"
        );
    }

    #[test]
    fn a_streak_of_delayed_connect_failures_stays_within_the_total_time_bound() {
        // The scaled witness: five 29 s connect-kind failures with their
        // backoffs would run 160 s under the old zero-charge policy, past
        // the 150 s budget. The measured policy charges each failure what
        // it awaited and stops the streak before the fifth attempt
        // crosses the budget.
        let wait = REQUEST_ALLOWANCE - Duration::from_secs(1);
        let mut streak = TransportStreak::default();
        let mut wall = Duration::ZERO;
        let mut stalled = false;
        for attempt in 1..=MAX_CONSECUTIVE_TRANSPORT_FAILURES {
            if !streak.may_reissue() {
                stalled = true;
                break;
            }
            let (backoff, is_stalled) = streak.record_failure(wait, false, true, "held".to_owned());
            wall += wait + backoff;
            assert!(
                streak.time_spent() <= MAX_TRANSPORT_STALL_TIME,
                "failure {attempt}: the charged time passed the bound"
            );
            stalled = stalled || is_stalled;
        }
        assert!(stalled, "the budget stopped the streak");
        assert_eq!(
            streak.failures(),
            4,
            "the fifth attempt would cross the budget, so it is never started"
        );
        assert_eq!(
            streak.time_spent(),
            Duration::from_secs(131),
            "4 x 29 s of measured wait plus 1 + 2 + 4 + 8 s of backoff"
        );
        assert!(
            wall <= MAX_TRANSPORT_STALL_TIME,
            "the real time the client spent is inside the budget: {wall:?}"
        );
    }

    #[test]
    fn a_late_body_failure_waits_its_allowance_and_buys_no_backoff() {
        // The HOLD counterexample shape: a truncated or delayed body is not
        // a connect-kind failure. It may have waited out the allowance, so
        // it charges the allowance and buys no backoff — five of it stay
        // inside the total time bound, where the old fast-failure
        // classification pushed the streak past it.
        let mut streak = TransportStreak::default();
        for attempt in 1..=MAX_CONSECUTIVE_TRANSPORT_FAILURES {
            let (backoff, stalled) =
                streak.record_failure(REQUEST_ALLOWANCE, false, false, "body cut short".to_owned());
            assert_eq!(backoff, Duration::ZERO, "failure {attempt} buys no backoff");
            assert_eq!(
                stalled,
                attempt == MAX_CONSECUTIVE_TRANSPORT_FAILURES,
                "the count bound fires on exactly the {attempt}th failure"
            );
        }
        assert_eq!(
            streak.time_spent(),
            MAX_TRANSPORT_STALL_TIME,
            "the allowance charges stay exactly at the total time bound"
        );
    }

    #[test]
    fn every_mixed_streak_stays_within_the_total_time_bound() {
        // All 3^5 interleavings of the three failure shapes: a timeout
        // that waits out its allowance, a delayed connect-kind failure
        // that consumed real time, and an instant refusal. Every streak
        // stalls, and the charged time never passes the bound.
        let shapes = [
            (REQUEST_ALLOWANCE, true, false),
            (HELD_HANDSHAKE_WAIT, false, true),
            (Duration::ZERO, false, true),
        ];
        for pattern in 0_u32..(3_u32.pow(MAX_CONSECUTIVE_TRANSPORT_FAILURES)) {
            let mut streak = TransportStreak::default();
            let mut wall = Duration::ZERO;
            let mut stalled = false;
            let mut digits = pattern;
            for position in 0..MAX_CONSECUTIVE_TRANSPORT_FAILURES {
                if !streak.may_reissue() {
                    stalled = true;
                    break;
                }
                let kind = digits % 3;
                digits /= 3;
                let (waited, is_timeout, is_connect) = shapes[kind as usize];
                let (backoff, is_stalled) =
                    streak.record_failure(waited, is_timeout, is_connect, "mixed".to_owned());
                wall += waited + backoff;
                assert!(
                    streak.time_spent() <= MAX_TRANSPORT_STALL_TIME,
                    "pattern {pattern} failure {position}: the charged time passed the bound"
                );
                stalled = stalled || is_stalled;
            }
            assert!(
                stalled,
                "pattern {pattern}: the streak stalled by the count or the budget"
            );
            assert!(
                wall <= MAX_TRANSPORT_STALL_TIME,
                "pattern {pattern}: the real time the client spent passed the bound: {wall:?}"
            );
        }
    }

    #[test]
    fn a_successful_reread_empties_the_streak() {
        let mut streak = TransportStreak::default();
        let (backoff, stalled) =
            streak.record_failure(Duration::ZERO, false, true, "refused".to_owned());
        assert!(!stalled && backoff > Duration::ZERO);
        assert_eq!(streak.last_error(), Some("refused"));
        streak.reset();
        assert_eq!(streak.failures(), 0);
        assert_eq!(streak.time_spent(), Duration::ZERO);
        assert_eq!(streak.last_error(), None);
        let mut is_stalled = false;
        for attempt in 0..MAX_CONSECUTIVE_TRANSPORT_FAILURES {
            let (_, stalled_now) =
                streak.record_failure(REQUEST_ALLOWANCE, true, false, "timed out".to_owned());
            is_stalled = stalled_now;
            assert_eq!(
                is_stalled,
                attempt + 1 == MAX_CONSECUTIVE_TRANSPORT_FAILURES,
                "after the reset the count starts over: failure {attempt} stalled={is_stalled}"
            );
        }
        assert!(is_stalled, "the reset streak stalls at the full count");
    }
}
