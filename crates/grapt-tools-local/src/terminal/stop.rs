//! Bounded stopping of a terminal root, shared by cancel, timeout and the
//! Windows Ctrl-C fallback.
//!
//! Every way a stop can go wrong ends inside a bound: the request cannot be
//! sent (kill now), it is sent but nothing happens (kill at the stop bound),
//! or the kill itself fails or has no effect (a settled failure at the reap
//! bound, which the supervisor records as unconfirmed cleanup with the
//! terminal's capacity still held). The root's exit is always observed
//! before any escalation, so an exit that races a stop is reported as that
//! exit and nothing is killed.
use std::time::Duration;
use tokio::time::Instant;

/// How long the supervisor waits for a stopped terminal to be observed
/// exiting before it settles cleanup as unconfirmed. It outlasts the whole
/// Windows stop chain (`stop`: stop bound, kill and reap, late report).
pub const STOP_SETTLE_BOUND: Duration = Duration::from_millis(4000);

/// How long the shim may take, after its shell exits or its job is
/// terminated, to see the job empty before it reports cleanup unconfirmed.
pub const CONFIRM_BOUND: Duration = Duration::from_millis(1500);
/// How long after the shim exits its report may still be in flight.
pub const REPORT_GRACE: Duration = Duration::from_millis(500);
/// How long a requested stop may take before the root is killed. It covers
/// the shim's whole confirmation allowance plus time to report and exit, so
/// a slow but valid confirmation is never cut short.
pub const STOP_BOUND: Duration = Duration::from_millis(2500);
/// How long a killed root may take to be observed exiting.
pub const REAP_BOUND: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopKind {
    /// Cancel, timeout or close.
    Terminate,
    /// A console Ctrl-C did not end the command.
    Interrupt,
}

/// The OS side of one terminal root.
pub trait Root {
    /// The root's exit status, without waiting.
    fn exit(&mut self) -> Result<Option<u32>, String>;
    /// Ask for a stop. `Ok(false)` when the request could not be delivered.
    fn request(&mut self, kind: StopKind) -> Result<bool, String>;
    /// The last resort.
    fn kill(&mut self) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Running,
    Exited(u32),
    /// The root could not be stopped within the bounds; it may still run.
    Failed(String),
}

#[derive(Debug, Default)]
enum Phase {
    #[default]
    Idle,
    Requested {
        deadline: Instant,
        terminate_sent: bool,
    },
    Killed {
        deadline: Instant,
        failure: Option<String>,
    },
}

#[derive(Debug, Default)]
pub struct Stop {
    phase: Phase,
}

impl Stop {
    /// Ask the root to stop. `Ok(false)` when it had already exited, so
    /// nothing was requested. A later stop never extends an earlier bound.
    pub fn request(
        &mut self,
        root: &mut impl Root,
        kind: StopKind,
        now: Instant,
    ) -> Result<bool, String> {
        // Only an observed exit makes a stop moot. A failed observation is
        // not an exit: the stop is still requested and its bound armed, and
        // a persisting failure settles through `step`.
        if matches!(root.exit(), Ok(Some(_))) {
            return Ok(false);
        }
        match &mut self.phase {
            Phase::Killed { .. } => return Ok(true),
            Phase::Requested { terminate_sent, .. } => {
                if kind == StopKind::Terminate && !*terminate_sent {
                    *terminate_sent = true;
                    if !matches!(root.request(kind), Ok(true)) {
                        self.kill(root, now);
                    }
                }
                return Ok(true);
            }
            Phase::Idle => {}
        }
        if matches!(root.request(kind), Ok(true)) {
            self.phase = Phase::Requested {
                deadline: now + STOP_BOUND,
                terminate_sent: kind == StopKind::Terminate,
            };
        } else {
            self.kill(root, now);
        }
        Ok(true)
    }

    /// Observe the root, escalating a stop whose bound has passed.
    pub fn step(&mut self, root: &mut impl Root, now: Instant) -> Step {
        match root.exit() {
            Ok(Some(code)) => return Step::Exited(code),
            Ok(None) => {}
            Err(error) => return Step::Failed(error),
        }
        match &self.phase {
            Phase::Requested { deadline, .. } if now >= *deadline => {
                self.kill(root, now);
                Step::Running
            }
            Phase::Killed { deadline, failure } if now >= *deadline => Step::Failed(match failure {
                Some(failure) => format!("terminal stop could not be confirmed: {failure}"),
                None => format!(
                    "terminal stop could not be confirmed: the root had not exited {} ms after it was killed",
                    REAP_BOUND.as_millis()
                ),
            }),
            _ => Step::Running,
        }
    }

    fn kill(&mut self, root: &mut impl Root, now: Instant) {
        self.phase = Phase::Killed {
            deadline: now + REAP_BOUND,
            failure: root.kill().err(),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A root with injectable faults. It exits when a delivered request is
    /// honoured, when a kill is effective, or on its own at `exits_at`.
    #[derive(Default)]
    #[expect(clippy::struct_excessive_bools, reason = "legacy")]
    struct FakeRoot {
        exit_errors: usize,
        exited: Option<u32>,
        deliver: bool,
        honour: bool,
        kill_fails: bool,
        kill_effective: bool,
        requests: Vec<StopKind>,
        kills: usize,
    }

    impl Root for FakeRoot {
        fn exit(&mut self) -> Result<Option<u32>, String> {
            if self.exit_errors > 0 {
                self.exit_errors -= 1;
                return Err("exit observation failed".into());
            }
            Ok(self.exited)
        }
        fn request(&mut self, kind: StopKind) -> Result<bool, String> {
            self.requests.push(kind);
            if self.deliver && self.honour {
                self.exited = Some(match kind {
                    StopKind::Terminate => 1,
                    StopKind::Interrupt => 0xC000_013A,
                });
            }
            Ok(self.deliver)
        }
        fn kill(&mut self) -> Result<(), String> {
            self.kills += 1;
            if self.kill_fails {
                return Err("access denied".into());
            }
            if self.kill_effective {
                self.exited = Some(1);
            }
            Ok(())
        }
    }

    fn working() -> FakeRoot {
        FakeRoot {
            deliver: true,
            honour: true,
            kill_effective: true,
            ..FakeRoot::default()
        }
    }

    #[test]
    fn an_honoured_interrupt_exits_with_the_ctrl_c_status_and_kills_nothing() {
        let now = Instant::now();
        let mut root = working();
        let mut stop = Stop::default();
        assert_eq!(stop.request(&mut root, StopKind::Interrupt, now), Ok(true));
        assert_eq!(stop.step(&mut root, now), Step::Exited(0xC000_013A));
        assert_eq!(root.kills, 0);
    }

    #[test]
    fn a_request_that_cannot_be_sent_kills_at_once() {
        // (a)
        let now = Instant::now();
        let mut root = FakeRoot {
            deliver: false,
            kill_effective: true,
            ..FakeRoot::default()
        };
        let mut stop = Stop::default();
        assert_eq!(stop.request(&mut root, StopKind::Interrupt, now), Ok(true));
        assert_eq!(root.kills, 1);
        assert_eq!(stop.step(&mut root, now), Step::Exited(1));
    }

    #[test]
    fn a_request_that_is_never_acted_on_is_killed_at_its_bound() {
        // (b) and (c): delivered but not consumed, or consumed without effect.
        let now = Instant::now();
        let mut root = FakeRoot {
            deliver: true,
            honour: false,
            kill_effective: true,
            ..FakeRoot::default()
        };
        let mut stop = Stop::default();
        assert_eq!(stop.request(&mut root, StopKind::Interrupt, now), Ok(true));
        assert_eq!(stop.step(&mut root, now + STOP_BOUND / 2), Step::Running);
        assert_eq!(root.kills, 0, "killed before the bound");
        assert_eq!(stop.step(&mut root, now + STOP_BOUND), Step::Running);
        assert_eq!(root.kills, 1);
        assert_eq!(stop.step(&mut root, now + STOP_BOUND), Step::Exited(1));
    }

    #[test]
    fn a_failed_or_ineffective_kill_settles_as_a_failure_at_the_reap_bound() {
        // (c): termination fails, or reports success and changes nothing.
        for kill_fails in [true, false] {
            let now = Instant::now();
            let mut root = FakeRoot {
                deliver: true,
                honour: false,
                kill_fails,
                kill_effective: false,
                ..FakeRoot::default()
            };
            let mut stop = Stop::default();
            assert_eq!(stop.request(&mut root, StopKind::Interrupt, now), Ok(true));
            let killed_at = now + STOP_BOUND;
            assert_eq!(stop.step(&mut root, killed_at), Step::Running);
            assert_eq!(
                stop.step(&mut root, killed_at + REAP_BOUND / 2),
                Step::Running
            );
            let Step::Failed(reason) = stop.step(&mut root, killed_at + REAP_BOUND) else {
                panic!("an unconfirmed kill did not settle");
            };
            assert!(reason.contains("could not be confirmed"), "{reason}");
            assert_eq!(reason.contains("access denied"), kill_fails, "{reason}");
            assert_eq!(root.kills, 1, "the kill was repeated");
        }
    }

    #[test]
    fn an_exit_that_races_the_fallback_is_reported_and_nothing_is_killed() {
        // (d): already exited when the fallback fires.
        let now = Instant::now();
        let mut root = FakeRoot {
            exited: Some(0),
            ..working()
        };
        let mut stop = Stop::default();
        assert_eq!(stop.request(&mut root, StopKind::Interrupt, now), Ok(false));
        assert!(root.requests.is_empty());
        // (d): exits on its own after an unconsumed request, before the bound.
        let mut root = FakeRoot {
            deliver: true,
            honour: false,
            kill_effective: true,
            ..FakeRoot::default()
        };
        let mut stop = Stop::default();
        assert_eq!(stop.request(&mut root, StopKind::Interrupt, now), Ok(true));
        root.exited = Some(0);
        assert_eq!(stop.step(&mut root, now + STOP_BOUND * 2), Step::Exited(0));
        assert_eq!(root.kills, 0, "an exited root was killed");
    }

    #[test]
    fn a_failed_exit_observation_still_arms_the_stop() {
        let now = Instant::now();
        let mut root = FakeRoot {
            exit_errors: 1,
            deliver: true,
            honour: false,
            kill_effective: true,
            ..FakeRoot::default()
        };
        let mut stop = Stop::default();
        assert_eq!(stop.request(&mut root, StopKind::Interrupt, now), Ok(true));
        assert_eq!(root.requests, vec![StopKind::Interrupt]);
        // Later observations succeed and show it still running: the bound holds.
        assert_eq!(stop.step(&mut root, now + STOP_BOUND / 2), Step::Running);
        assert_eq!(stop.step(&mut root, now + STOP_BOUND), Step::Running);
        assert_eq!(root.kills, 1);
    }

    #[test]
    fn a_late_but_permitted_confirmation_is_never_killed() {
        // The shim terminated the job and is still waiting, inside its own
        // allowance, for it to empty; it then reports and exits.
        let now = Instant::now();
        let mut root = FakeRoot {
            deliver: true,
            honour: false,
            kill_effective: true,
            ..FakeRoot::default()
        };
        let mut stop = Stop::default();
        assert_eq!(stop.request(&mut root, StopKind::Terminate, now), Ok(true));
        let confirmed = now + CONFIRM_BOUND;
        assert_eq!(stop.step(&mut root, confirmed), Step::Running);
        root.exited = Some(1);
        assert_eq!(
            stop.step(&mut root, confirmed + REPORT_GRACE),
            Step::Exited(1)
        );
        assert_eq!(root.kills, 0, "a valid confirmation was cut short");
    }

    #[test]
    fn the_nested_budgets_are_ordered() {
        // The stop bound outlasts the shim's whole confirmation and report.
        assert!(STOP_BOUND >= CONFIRM_BOUND + REPORT_GRACE);
        // The supervisor waits out a stop, a kill and a late report.
        assert!(STOP_SETTLE_BOUND > STOP_BOUND + REAP_BOUND + REPORT_GRACE);
    }

    #[test]
    fn a_timeout_during_the_fallback_also_terminates_without_extending_the_bound() {
        // (e)
        let now = Instant::now();
        let mut root = FakeRoot {
            deliver: true,
            honour: false,
            kill_effective: true,
            ..FakeRoot::default()
        };
        let mut stop = Stop::default();
        assert_eq!(stop.request(&mut root, StopKind::Interrupt, now), Ok(true));
        let later = now + STOP_BOUND / 2;
        assert_eq!(
            stop.request(&mut root, StopKind::Terminate, later),
            Ok(true)
        );
        assert_eq!(
            root.requests,
            vec![StopKind::Interrupt, StopKind::Terminate]
        );
        // The original bound still applies.
        assert_eq!(stop.step(&mut root, now + STOP_BOUND), Step::Running);
        assert_eq!(root.kills, 1);
        // A second timeout or cancel adds nothing.
        assert_eq!(
            stop.request(&mut root, StopKind::Terminate, now + STOP_BOUND),
            Ok(false)
        );
        assert_eq!(root.requests.len(), 2);
    }
}
