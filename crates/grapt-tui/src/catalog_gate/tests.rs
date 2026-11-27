use super::{CatalogGate, Gate};
use grapt_client::FramePoll;
use std::time::Duration;

fn frame(text: &str) -> Option<FramePoll> {
    Some(FramePoll::Frame(text.into()))
}

#[test]
fn a_slow_refresh_that_succeeds_never_drops_the_held_frame() {
    let mut gate = CatalogGate::with_backstop(Duration::from_secs(30));
    assert_eq!(gate.admit(frame("held"), true), Gate::Hold);
    std::thread::sleep(Duration::from_millis(30));
    // Slow attempts and fewer failures than the limit keep holding.
    for _ in 0..2 {
        gate.refreshed(false);
        let held = gate.take();
        assert_eq!(gate.admit(held, true), Gate::Hold);
    }
    gate.refreshed(true);
    let held = gate.take();
    assert_eq!(gate.admit(held, false), Gate::Pass(frame("held")));
}

#[test]
fn three_failed_refreshes_in_a_row_give_way_to_a_resync() {
    let mut gate = CatalogGate::default();
    assert_eq!(gate.admit(frame("held"), true), Gate::Hold);
    for _ in 0..3 {
        gate.refreshed(false);
    }
    let held = gate.take();
    assert_eq!(gate.admit(held, true), Gate::Resync);
    assert_eq!(gate.take(), None);
    // The count starts again after the resync.
    assert_eq!(gate.admit(frame("next"), true), Gate::Hold);
}

#[test]
fn a_success_between_failures_restarts_the_count() {
    let mut gate = CatalogGate::default();
    for succeeded in [false, false, true, false, false] {
        gate.refreshed(succeeded);
    }
    assert_eq!(gate.admit(frame("held"), true), Gate::Hold);
}

#[test]
fn a_refresh_that_hangs_resyncs_at_the_backstop() {
    let mut gate = CatalogGate::with_backstop(Duration::from_millis(10));
    assert_eq!(gate.admit(frame("held"), true), Gate::Hold);
    std::thread::sleep(Duration::from_millis(20));
    let held = gate.take();
    assert_eq!(gate.admit(held, true), Gate::Resync);
}

#[test]
fn a_reset_starts_the_next_hold_on_its_own_clock() {
    let mut gate = CatalogGate::with_backstop(Duration::from_millis(10));
    assert_eq!(gate.admit(frame("old"), true), Gate::Hold);
    std::thread::sleep(Duration::from_millis(20));
    gate.reset();
    assert_eq!(gate.holding_since(), None);
    assert_eq!(gate.take(), None);
    assert_eq!(gate.admit(frame("new"), true), Gate::Hold);
}

#[test]
fn nothing_to_hold_never_resyncs() {
    let mut gate = CatalogGate::default();
    for _ in 0..5 {
        gate.refreshed(false);
    }
    assert_eq!(gate.admit(None, true), Gate::Hold);
    assert_eq!(gate.admit(None, false), Gate::Pass(None));
}
