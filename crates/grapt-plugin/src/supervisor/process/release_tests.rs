//! The bounded release of a held child (review HOLD on 39f5f9f9): a
//! child that never exits, or one that cannot be killed, ends the release
//! within the bound, reported and still held. Fakes only; nothing is signalled.

use super::*;

struct Fake {
    exits_after: Option<u32>,
    polls: u32,
    kill_fails: bool,
}

impl Reapable for Fake {
    fn kill(&mut self) -> std::io::Result<()> {
        if self.kill_fails {
            Err(std::io::Error::other("operation not permitted"))
        } else {
            Ok(())
        }
    }

    fn try_wait(&mut self) -> std::io::Result<bool> {
        self.polls += 1;
        Ok(self.exits_after.is_some_and(|after| self.polls >= after))
    }
}

fn held(exits_after: Option<u32>, kill_fails: bool) -> Option<Fake> {
    Some(Fake {
        exits_after,
        polls: 0,
        kill_fails,
    })
}

#[test]
fn a_reaped_child_is_released() {
    let mut child = held(Some(2), false);
    assert!(release(&mut child, Duration::from_secs(5), "sentinel").is_ok());
    assert!(child.is_none(), "custody outlived the observed reap");
}

#[test]
fn a_child_that_never_exits_is_reported_within_the_bound_and_stays_held() {
    let mut child = held(None, false);
    let bound = Duration::from_millis(100);
    let started = tokio::time::Instant::now();
    let outcome = release(&mut child, bound, "sentinel of process group 500");
    assert!(
        tokio::time::Instant::now().duration_since(started) < bound + Duration::from_secs(1),
        "the release outlived its bound"
    );
    assert!(
        matches!(&outcome, Err(PluginError::SurvivesKill(reason)) if reason.contains("sentinel of process group 500")),
        "{outcome:?}"
    );
    assert!(child.is_some(), "an unreaped child was let go");
}

#[test]
fn a_failed_kill_is_reported_and_the_child_stays_held() {
    let mut child = held(Some(1), true);
    let outcome = release(&mut child, Duration::from_secs(5), "sentinel");
    assert!(
        matches!(&outcome, Err(PluginError::Protocol(reason)) if reason.contains("could not be killed")),
        "{outcome:?}"
    );
    assert!(child.is_some(), "a child that was not killed was let go");
    assert_eq!(
        child.map(|fake| fake.polls),
        Some(0),
        "a failed kill was waited on"
    );
}

/// A fake that counts its drops, so a test can prove the handle stayed owned.
struct Counted {
    fake: Fake,
    drops: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    exits: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Reapable for Counted {
    fn kill(&mut self) -> std::io::Result<()> {
        self.fake.kill()
    }

    fn try_wait(&mut self) -> std::io::Result<bool> {
        Ok(self.exits.load(std::sync::atomic::Ordering::SeqCst))
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.drops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The review HOLD on 7a2af7d3, through the startup-error path: children a
/// failed start cannot reap (one never exits, one cannot even be killed) are
/// transferred, not dropped, and the start's admission stays charged so no
/// successor is admitted until the sweep observes every reap.
#[test]
fn a_failed_starts_unreaped_children_stay_owned_and_keep_their_admission_charged() {
    use crate::protocol::DEFAULT_SUPERVISOR_BUDGET;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let budget = ProcessBudget::new(SupervisorBudget {
        processes: 2,
        ..DEFAULT_SUPERVISOR_BUDGET
    })
    .unwrap_or_else(|error| panic!("budget: {error}"));
    let admission = budget
        .reserve(1)
        .unwrap_or_else(|error| panic!("admission: {error}"))
        .pop()
        .unwrap_or_else(|| panic!("one admission"));
    let drops = Arc::new(AtomicUsize::new(0));
    let exits = Arc::new(AtomicBool::new(false));
    let child = |kill_fails| Counted {
        fake: Fake {
            exits_after: None,
            polls: 0,
            kill_fails,
        },
        drops: Arc::clone(&drops),
        exits: Arc::clone(&exits),
    };
    let stranded = Stranded::default();
    let started = tokio::time::Instant::now();
    let outcome = abandon_children(
        vec![child(false), child(true)],
        Duration::from_millis(100),
        &stranded,
        7,
    );
    assert!(tokio::time::Instant::now().duration_since(started) < Duration::from_secs(2));
    assert!(
        matches!(&outcome, Err(PluginError::Protocol(reason)) if reason.contains("held until reaped")),
        "{outcome:?}"
    );
    assert_eq!(stranded.held(), 2);
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "a stranded child was dropped"
    );
    // The caller settles the failed start: its admission stays charged.
    assert!(stranded.charge(7, admission).is_none());
    assert_eq!(budget.live(), 1);
    assert!(
        budget.reserve(2).is_err(),
        "a full successor was admitted past an unreaped child"
    );
    stranded.sweep();
    assert_eq!((stranded.held(), budget.live()), (2, 1));
    // Both are reaped: the sweep releases them and their admission.
    exits.store(true, Ordering::SeqCst);
    stranded.sweep();
    assert_eq!((stranded.held(), budget.live()), (0, 0));
    assert_eq!(drops.load(Ordering::SeqCst), 2);
}

#[test]
fn a_failed_start_with_nothing_stranded_hands_its_admission_back() {
    use crate::protocol::DEFAULT_SUPERVISOR_BUDGET;
    let budget = ProcessBudget::new(SupervisorBudget {
        processes: 2,
        ..DEFAULT_SUPERVISOR_BUDGET
    })
    .unwrap_or_else(|error| panic!("budget: {error}"));
    let admission = budget
        .reserve(1)
        .unwrap_or_else(|error| panic!("admission: {error}"))
        .pop()
        .unwrap_or_else(|| panic!("one admission"));
    let stranded = Stranded::<Fake>::default();
    assert!(abandon_children(
        vec![Fake {
            exits_after: Some(1),
            polls: 0,
            kill_fails: false
        }],
        Duration::from_secs(1),
        &stranded,
        3
    )
    .is_ok());
    let returned = stranded.charge(3, admission);
    assert!(returned.is_some());
    drop(returned);
    assert_eq!(budget.live(), 0);
}

/// The review HOLD on bcba9142: a test-owned child whose release fails is kept
/// and reported, never dropped as if it had ended.
#[test]
fn held_children_that_survive_their_release_are_kept_and_reported() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let drops = Arc::new(AtomicUsize::new(0));
    let exits = Arc::new(AtomicBool::new(false));
    let child = |kill_fails| Counted {
        fake: Fake {
            exits_after: None,
            polls: 0,
            kill_fails,
        },
        drops: Arc::clone(&drops),
        exits: Arc::clone(&exits),
    };
    let mut held = Held(vec![child(false), child(true)]);
    let outcome = held.finish(Duration::from_millis(50));
    assert!(
        matches!(&outcome, Err(PluginError::Protocol(reason)) if reason.contains("2 test-owned child(ren) survived")),
        "{outcome:?}"
    );
    assert_eq!((held.0.len(), drops.load(Ordering::SeqCst)), (2, 0));
    exits.store(true, Ordering::SeqCst);
    held.reap_some();
    assert_eq!((held.0.len(), drops.load(Ordering::SeqCst)), (0, 2));
}
