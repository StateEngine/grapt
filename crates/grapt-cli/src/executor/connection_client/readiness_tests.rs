//! The election loser's readiness poll is bounded and probes at its end.
use super::*;
use std::cell::Cell;

#[tokio::test(start_paused = true)]
async fn a_winner_that_becomes_ready_within_the_window_is_borrowed() {
    let probes = Cell::new(0);
    let ready = poll_ready(Duration::from_secs(5), || {
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
async fn the_poll_ends_at_its_window_with_a_last_probe_there() {
    let started = tokio::time::Instant::now();
    let last = Cell::new(started);
    let ready: Option<()> = poll_ready(Duration::from_secs(5), || {
        last.set(tokio::time::Instant::now());
        async { Attempt::Pending }
    })
    .await;
    assert_eq!(ready, None);
    assert_eq!(
        last.get() - started,
        Duration::from_secs(5),
        "probed at the window"
    );
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(5),
        "no pause after it"
    );
}

#[tokio::test(start_paused = true)]
async fn a_slow_probe_cannot_outlast_the_window() {
    let started = tokio::time::Instant::now();
    let ready: Option<()> = poll_ready(Duration::from_secs(5), || async {
        tokio::time::sleep(Duration::from_secs(2)).await;
        Attempt::Pending
    })
    .await;
    assert_eq!(ready, None);
    assert!(started.elapsed() <= Duration::from_secs(5));
}

#[tokio::test(start_paused = true)]
async fn a_refusal_stops_the_poll_at_once() {
    let started = tokio::time::Instant::now();
    let ready: Option<()> = poll_ready(Duration::from_secs(5), || async { Attempt::Stop }).await;
    assert_eq!(ready, None);
    assert_eq!(started.elapsed(), Duration::ZERO);
}
