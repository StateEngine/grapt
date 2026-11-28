//! The election loser's readiness poll is bounded, leaves its last probe time
//! to finish, and uses no answer that comes after its window.
use super::*;
use std::cell::Cell;

const WINDOW: Duration = Duration::from_secs(10);
const ALLOWANCE: Duration = Duration::from_secs(5);

#[tokio::test(start_paused = true)]
async fn a_winner_that_becomes_ready_within_the_window_is_borrowed() {
    let probes = Cell::new(0);
    let ready = poll_ready(WINDOW, ALLOWANCE, || {
        probes.set(probes.get() + 1);
        let attempt = if probes.get() < 4 {
            Attempt::Pending
        } else {
            Attempt::Ready(probes.get())
        };
        async move { attempt }
    })
    .await;
    assert_eq!(ready, Some(4));
}

#[tokio::test(start_paused = true)]
async fn the_last_probe_starts_with_its_allowance_left() {
    let started = tokio::time::Instant::now();
    let last = Cell::new(started);
    let ready: Option<()> = poll_ready(WINDOW, ALLOWANCE, || {
        last.set(tokio::time::Instant::now());
        async { Attempt::Pending }
    })
    .await;
    assert_eq!(ready, None);
    assert_eq!(
        last.get() - started,
        WINDOW - ALLOWANCE,
        "the last probe had no time"
    );
    assert_eq!(started.elapsed(), WINDOW - ALLOWANCE, "no pause after it");
}

#[tokio::test(start_paused = true)]
async fn a_winner_ready_just_before_the_last_start_is_borrowed_in_time() {
    // Ready from 4.95 seconds on; each probe's health check takes 4.5 seconds
    // once the winner answers, which the last probe has time for.
    let started = tokio::time::Instant::now();
    let ready = poll_ready(WINDOW, ALLOWANCE, || async move {
        if started.elapsed() < Duration::from_millis(4_950) {
            return Attempt::Pending;
        }
        tokio::time::sleep(Duration::from_millis(4_500)).await;
        Attempt::Ready(())
    })
    .await;
    assert_eq!(ready, Some(()));
    assert!(started.elapsed() <= WINDOW);
}

#[tokio::test(start_paused = true)]
async fn an_answer_after_the_window_is_not_used() {
    let ready = poll_ready(WINDOW, ALLOWANCE, || async {
        // A probe that cannot be cut short: its answer lands past the window.
        tokio::time::advance(WINDOW + Duration::from_secs(1)).await;
        Attempt::Ready(())
    })
    .await;
    assert_eq!(ready, None);
}

#[tokio::test(start_paused = true)]
async fn a_slow_probe_cannot_outlast_the_window() {
    let started = tokio::time::Instant::now();
    let ready: Option<()> = poll_ready(WINDOW, ALLOWANCE, || async {
        tokio::time::sleep(Duration::from_secs(6)).await;
        Attempt::Pending
    })
    .await;
    assert_eq!(ready, None);
    assert!(started.elapsed() <= WINDOW);
}

#[tokio::test(start_paused = true)]
async fn a_refusal_stops_the_poll_at_once() {
    let started = tokio::time::Instant::now();
    let ready: Option<()> = poll_ready(WINDOW, ALLOWANCE, || async { Attempt::Stop }).await;
    assert_eq!(ready, None);
    assert_eq!(started.elapsed(), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn a_probe_pending_at_the_last_start_is_followed_by_one_more() {
    // The first probe runs until exactly the last start; the winner is ready
    // for the probe that follows it.
    let started = tokio::time::Instant::now();
    let probes = Cell::new(0);
    let ready = poll_ready(WINDOW, ALLOWANCE, || {
        probes.set(probes.get() + 1);
        let first = probes.get() == 1;
        async move {
            if first {
                tokio::time::sleep(WINDOW - ALLOWANCE).await;
                Attempt::Pending
            } else {
                Attempt::Ready(())
            }
        }
    })
    .await;
    assert_eq!(ready, Some(()), "stopped while the allowance remained");
    assert_eq!(started.elapsed(), WINDOW - ALLOWANCE);
}
