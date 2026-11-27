//! Every refusal is witnessed with recorded fakes: no test here sends a real
//! signal to anything.

use super::*;
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Clone, Default)]
struct Table {
    seen: BTreeMap<u32, Seen>,
    groups: BTreeMap<u32, u32>,
}

impl Probe for Table {
    fn observe(&self, pid: u32) -> Seen {
        self.seen.get(&pid).copied().unwrap_or(Seen::Gone)
    }

    fn group_of(&self, pid: u32) -> Option<u32> {
        self.groups.get(&pid).copied()
    }
}

#[derive(Default)]
struct Recorded(Vec<(&'static str, i32)>);

impl Signals for Recorded {
    fn kill(&mut self, pid: i32) -> Result<(), String> {
        self.0.push(("kill", pid));
        Ok(())
    }

    fn kill_group(&mut self, group: i32) -> Result<(), String> {
        self.0.push(("killpg", group));
        Ok(())
    }
}

const OWN: Own = Own {
    pid: 4000,
    group: 3900,
};

/// Leader 500 started at 10; its child 501 at 11; a grandchild 502 at 12,
/// recorded under 501; the pinned sentinel 600 is in group 500.
fn bridge() -> Table {
    let alive = |started, parent| Seen::Alive {
        started,
        parent: Some(parent),
    };
    Table {
        seen: BTreeMap::from([
            (500, alive(10, 50)),
            (501, alive(11, 500)),
            (502, alive(12, 501)),
        ]),
        groups: BTreeMap::from([(500, 500), (501, 500), (600, 500)]),
    }
}

fn grandchild() -> Target {
    Target::Process {
        identity: Identity {
            pid: 502,
            started: 12,
            parent: Some(501),
            parent_started: Some(11),
        },
        floor: 10,
    }
}

fn run(target: Target, table: &Table) -> (Result<Sent, Refusal>, Vec<(&'static str, i32)>) {
    let mut recorded = Recorded::default();
    let outcome = signal(target, OWN, table, &mut recorded);
    (outcome, recorded.0)
}

#[test]
fn a_verified_process_is_signalled_and_one_already_gone_is_not_an_error() {
    assert_eq!(
        run(grandchild(), &bridge()),
        (Ok(Sent::Signalled), vec![("kill", 502)])
    );
    let mut gone = bridge();
    gone.seen.remove(&502);
    assert_eq!(run(grandchild(), &gone), (Ok(Sent::Gone), vec![]));
}

#[test]
fn an_identity_that_changed_between_the_tick_and_the_kill_is_refused_at_re_sample() {
    // Recorded at 12 by the last tick; re-read now, the PID runs a process from 13.
    let mut table = bridge();
    table.seen.insert(
        502,
        Seen::Alive {
            started: 13,
            parent: Some(501),
        },
    );
    assert_eq!(
        run(grandchild(), &table),
        (Err(Refusal::Reused(502)), vec![])
    );
}

#[test]
fn a_process_that_started_before_the_leader_is_refused() {
    let Target::Process { identity, .. } = grandchild() else {
        panic!("fixture target is a process")
    };
    let early = Target::Process {
        identity,
        floor: 20,
    };
    assert_eq!(
        run(early, &bridge()),
        (Err(Refusal::BeforeLeader(502)), vec![])
    );
}

#[test]
fn a_parent_mismatch_or_a_reused_recorded_parent_is_refused() {
    let mut moved = bridge();
    moved.seen.insert(
        502,
        Seen::Alive {
            started: 12,
            parent: Some(777),
        },
    );
    assert_eq!(
        run(grandchild(), &moved),
        (Err(Refusal::ParentChanged(502)), vec![])
    );
    // Same parent PID, but the parent itself is a different process now.
    let mut reused_parent = bridge();
    reused_parent.seen.insert(
        501,
        Seen::Alive {
            started: 99,
            parent: Some(500),
        },
    );
    assert_eq!(
        run(grandchild(), &reused_parent),
        (Err(Refusal::ParentChanged(502)), vec![])
    );
}

#[test]
fn an_escapee_launchd_adopted_is_signalled_on_its_own_identity() {
    let mut escaped = bridge();
    escaped.seen.remove(&501);
    escaped.seen.insert(
        502,
        Seen::Alive {
            started: 12,
            parent: Some(1),
        },
    );
    assert_eq!(
        run(grandchild(), &escaped),
        (Ok(Sent::Signalled), vec![("kill", 502)])
    );
}

#[test]
fn zero_one_and_wrapping_ids_are_refused_for_processes_and_groups() {
    for id in [0, 1, u32::MAX, 1 << 31] {
        let process = Target::Process {
            identity: Identity {
                pid: id,
                started: 12,
                parent: Some(501),
                parent_started: Some(11),
            },
            floor: 0,
        };
        assert_eq!(
            run(process, &bridge()),
            (Err(Refusal::NotABridgeId(id)), vec![])
        );
        let group = Target::Group {
            group: id,
            holder: 600,
        };
        assert_eq!(
            run(group, &bridge()),
            (Err(Refusal::NotABridgeId(id)), vec![])
        );
    }
    // A fixture's "ready 0" line is a parse error, never a PID.
    assert_eq!(bridge_id(0, OWN), Err(Refusal::NotABridgeId(0)));
}

#[test]
fn the_supervisors_own_process_and_group_are_refused_even_when_forged() {
    let mut table = bridge();
    table.seen.insert(
        OWN.pid,
        Seen::Alive {
            started: 1,
            parent: Some(1),
        },
    );
    table.groups.insert(600, OWN.group);
    let own_process = Target::Process {
        identity: Identity {
            pid: OWN.pid,
            started: 1,
            parent: Some(1),
            parent_started: None,
        },
        floor: 0,
    };
    assert_eq!(
        run(own_process, &table),
        (Err(Refusal::Own(OWN.pid)), vec![])
    );
    let forged = Target::Group {
        group: OWN.group,
        holder: 600,
    };
    assert_eq!(run(forged, &table), (Err(Refusal::Own(OWN.group)), vec![]));
}

#[test]
fn an_unreadable_identity_is_reported_unverified_and_never_signalled() {
    let mut table = bridge();
    table.seen.insert(502, Seen::Unreadable);
    assert_eq!(
        run(grandchild(), &table),
        (Err(Refusal::Unverified(502)), vec![])
    );
}

#[test]
fn a_group_is_signalled_only_while_our_unreaped_holder_is_in_it() {
    let group = Target::Group {
        group: 500,
        holder: 600,
    };
    assert_eq!(
        run(group, &bridge()),
        (Ok(Sent::Signalled), vec![("killpg", 500)])
    );
    // Second generation: the original group emptied and its ID was reused by
    // a new group our holder is not in. No membership claim authorizes it.
    let mut reused = bridge();
    reused.groups.insert(600, 600);
    assert_eq!(run(group, &reused), (Err(Refusal::Unverified(500)), vec![]));
    reused.groups.remove(&600);
    assert_eq!(run(group, &reused), (Err(Refusal::Unverified(500)), vec![]));
}

/// The grandchild reads as unreadable for its first `unreadable` reads, then
/// as `then`: a zombie its parent or launchd reaps a little later.
struct Settling {
    table: Table,
    unreadable: std::cell::Cell<usize>,
    then: Seen,
}

impl Probe for Settling {
    fn observe(&self, pid: u32) -> Seen {
        if pid != 502 {
            return self.table.observe(pid);
        }
        match self.unreadable.get() {
            0 => self.then,
            left => {
                self.unreadable.set(left - 1);
                Seen::Unreadable
            }
        }
    }

    fn group_of(&self, pid: u32) -> Option<u32> {
        self.table.group_of(pid)
    }
}

fn settle(
    target: Target,
    probe: &impl Probe,
    bound: Duration,
) -> (Result<Sent, Refusal>, Vec<(&'static str, i32)>, Duration) {
    let mut recorded = Recorded::default();
    let started = tokio::time::Instant::now();
    let outcome = signal_settled(target, OWN, probe, &mut recorded, started + bound);
    (outcome, recorded.0, started.elapsed())
}

/// The macOS run of 90f648f1: members a group kill just ended read as
/// unreadable until reaped. Re-read within the bound, they settle as gone,
/// or, if still alive under their identity, are signalled.
#[test]
fn an_unreadable_process_is_re_read_within_the_bound_and_settles() {
    let reaped = Settling {
        table: bridge(),
        unreadable: std::cell::Cell::new(3),
        then: Seen::Gone,
    };
    let (outcome, signals, _) = settle(grandchild(), &reaped, Duration::from_secs(5));
    assert_eq!((outcome, signals), (Ok(Sent::Gone), vec![]));
    let alive = Settling {
        table: bridge(),
        unreadable: std::cell::Cell::new(3),
        then: Seen::Alive {
            started: 12,
            parent: Some(501),
        },
    };
    let (outcome, signals, _) = settle(grandchild(), &alive, Duration::from_secs(5));
    assert_eq!(
        (outcome, signals),
        (Ok(Sent::Signalled), vec![("kill", 502)])
    );
}

#[test]
fn a_process_still_unreadable_at_the_bound_is_refused_and_a_group_is_never_retried() {
    let stuck = Settling {
        table: bridge(),
        unreadable: std::cell::Cell::new(usize::MAX),
        then: Seen::Gone,
    };
    let bound = Duration::from_millis(100);
    let (outcome, signals, elapsed) = settle(grandchild(), &stuck, bound);
    assert_eq!((outcome, signals), (Err(Refusal::Unverified(502)), vec![]));
    assert!(
        elapsed >= bound && elapsed < bound + Duration::from_secs(2),
        "{elapsed:?}"
    );
    let mut table = bridge();
    table.groups.remove(&600);
    let (outcome, signals, elapsed) = settle(
        Target::Group {
            group: 500,
            holder: 600,
        },
        &table,
        Duration::from_secs(10),
    );
    assert_eq!((outcome, signals), (Err(Refusal::Unverified(500)), vec![]));
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
}

/// The second macOS run (66801725): an orphan killed by identity stays an
/// unreaped zombie past any bound. A zombie answers a null signal but not a
/// group lookup; it is dead, so it reads as gone. Every other answer stays
/// unreadable.
#[test]
fn only_a_pid_the_group_lookup_cannot_find_after_a_null_signal_did_is_a_zombie() {
    use Lookup::{Failed, Found, Missing};
    for (null_signal, group, seen) in [
        (Missing, Missing, Seen::Gone),
        (Missing, Found, Seen::Gone),
        (Missing, Failed, Seen::Gone),
        (Found, Missing, Seen::Gone),
        (Found, Found, Seen::Unreadable),
        (Found, Failed, Seen::Unreadable),
        (Failed, Missing, Seen::Unreadable),
        (Failed, Found, Seen::Unreadable),
        (Failed, Failed, Seen::Unreadable),
    ] {
        assert_eq!(
            unlisted(null_signal, group),
            seen,
            "{null_signal:?} {group:?}"
        );
    }
}
