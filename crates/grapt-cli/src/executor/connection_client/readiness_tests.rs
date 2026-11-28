//! The election loser's readiness poll keeps probing until its window ends,
//! bounds every probe by what is left, and uses no answer that comes after.
use super::*;
use std::cell::Cell;

const WINDOW: Duration = Duration::from_secs(10);

#[tokio::test(start_paused = true)]
async fn a_winner_that_becomes_ready_within_the_window_is_borrowed() {
    let probes = Cell::new(0);
    let ready = poll_ready(WINDOW, || {
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
async fn a_slow_pending_probe_past_five_seconds_is_followed_by_another() {
    // The first probe stalls until 6.5 seconds; the winner is ready for the
    // next one, well inside the window.
    let started = tokio::time::Instant::now();
    let probes = Cell::new(0);
    let ready = poll_ready(WINDOW, || {
        probes.set(probes.get() + 1);
        let first = probes.get() == 1;
        async move {
            if first {
                tokio::time::sleep(Duration::from_millis(6_500)).await;
                Attempt::Pending
            } else {
                tokio::time::sleep(Duration::from_millis(500)).await;
                Attempt::Ready(())
            }
        }
    })
    .await;
    assert_eq!(ready, Some(()), "stopped with time left in the window");
    assert!(started.elapsed() < WINDOW);
}

#[tokio::test(start_paused = true)]
async fn probing_stops_at_the_window_with_no_pause_after_it() {
    let started = tokio::time::Instant::now();
    let ready: Option<()> = poll_ready(WINDOW, || async { Attempt::Pending }).await;
    assert_eq!(ready, None);
    assert_eq!(started.elapsed(), WINDOW);
}

#[tokio::test(start_paused = true)]
async fn no_probe_starts_when_the_pause_ends_at_the_deadline() {
    let probes = Cell::new(0);
    let ready: Option<()> = poll_ready(WINDOW, || {
        probes.set(probes.get() + 1);
        async {
            tokio::time::sleep(WINDOW - Duration::from_millis(50)).await;
            Attempt::Pending
        }
    })
    .await;
    assert_eq!(ready, None);
    assert_eq!(probes.get(), 1, "a probe was started at the deadline");
}

#[tokio::test(start_paused = true)]
async fn a_slow_probe_cannot_outlast_the_window() {
    let started = tokio::time::Instant::now();
    let ready: Option<()> = poll_ready(WINDOW, || async {
        tokio::time::sleep(Duration::from_secs(60)).await;
        Attempt::Pending
    })
    .await;
    assert_eq!(ready, None);
    assert_eq!(started.elapsed(), WINDOW);
}

#[tokio::test(start_paused = true)]
async fn an_answer_after_the_window_is_not_used() {
    let ready = poll_ready(WINDOW, || async {
        // A probe that cannot be cut short: its answer lands past the window.
        tokio::time::advance(WINDOW + Duration::from_secs(1)).await;
        Attempt::Ready(())
    })
    .await;
    assert_eq!(ready, None);
}

#[tokio::test(start_paused = true)]
async fn a_refusal_stops_the_poll_at_once() {
    let started = tokio::time::Instant::now();
    let ready: Option<()> = poll_ready(WINDOW, || async { Attempt::Stop }).await;
    assert_eq!(ready, None);
    assert_eq!(started.elapsed(), Duration::ZERO);
}
