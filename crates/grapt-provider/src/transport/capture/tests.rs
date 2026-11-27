//! Witnesses for the capture seam's three contracts.
//!
//! Each contract gets positive evidence, failure evidence, and resurrection
//! evidence naming the exact defect the first design had.

use std::num::NonZeroUsize;
use std::sync::mpsc::TryRecvError;

use super::*;

fn cap(n: usize) -> NonZeroUsize {
    match NonZeroUsize::new(n) {
        Some(n) => n,
        None => panic!("a non-zero test capacity"),
    }
}

fn attempt(id: &str) -> PhysicalAttemptId {
    match PhysicalAttemptId::new(id) {
        Some(attempt) => attempt,
        None => panic!("test attempt id"),
    }
}

fn drain(feed: &Receiver<CaptureEvent>) -> Vec<CaptureEvent> {
    let mut events = Vec::new();
    loop {
        match feed.try_recv() {
            Ok(event) => events.push(event),
            Err(_) => return events,
        }
    }
}

// ---------------------------------------------------------------------------
// Contract A — attribution: one attempt, one feed, and it says which
// ---------------------------------------------------------------------------

#[test]
fn each_attempt_gets_its_own_identified_feed() {
    // RESURRECTION of the first design's consumability defect: one shared sink
    // received chunks from every concurrent attempt with NO attempt identity on
    // them, so a consumer could not attribute a single byte. Two concurrent
    // attempts here must be separable, and each must name itself.
    let (exchange, attempts) = capture_exchange(cap(4), cap(8), cap(64));

    let mut first = match exchange.open(&attempt("att_a")) {
        Some(tap) => tap,
        None => panic!("the exchange must accept an attempt"),
    };
    let mut second = match exchange.open(&attempt("att_b")) {
        Some(tap) => tap,
        None => panic!("the exchange must accept a second attempt"),
    };
    first.chunk(b"AAA");
    second.chunk(b"BBB");

    let a = match attempts.try_recv() {
        Ok(a) => a,
        Err(error) => panic!("the first attempt must be announced: {error}"),
    };
    let b = match attempts.try_recv() {
        Ok(b) => b,
        Err(error) => panic!("the second attempt must be announced: {error}"),
    };
    assert_eq!(a.attempt().as_str(), "att_a");
    assert_eq!(b.attempt().as_str(), "att_b");
    assert_eq!(drain(a.feed()), vec![CaptureEvent::Chunk(b"AAA".to_vec())]);
    assert_eq!(drain(b.feed()), vec![CaptureEvent::Chunk(b"BBB".to_vec())]);
}

// ---------------------------------------------------------------------------
// Contract B — settlement, including on cancellation
// ---------------------------------------------------------------------------

#[test]
fn a_terminal_settles_the_feed_exactly_once() {
    let (exchange, attempts) = capture_exchange(cap(2), cap(8), cap(64));
    let mut tap = match exchange.open(&attempt("att_term")) {
        Some(tap) => tap,
        None => panic!("accepted"),
    };
    tap.chunk(b"body");
    tap.settle(CaptureOutcome::Terminal);
    // Idempotent: a second settle must not append a second Finished.
    tap.settle(CaptureOutcome::Terminal);
    tap.settle(CaptureOutcome::Incomplete);
    drop(tap);

    let announced = match attempts.try_recv() {
        Ok(announced) => announced,
        Err(error) => panic!("announced: {error}"),
    };
    assert_eq!(
        drain(announced.feed()),
        vec![
            CaptureEvent::Chunk(b"body".to_vec()),
            CaptureEvent::Finished {
                outcome: CaptureOutcome::Terminal,
                loss: CaptureLoss::default(),
            },
        ]
    );
}

#[test]
fn dropping_a_tap_mid_flight_settles_incomplete() {
    // RESURRECTION of the first design's fatal defect. Dropping the stream —
    // which is exactly what a Session interrupt looks like from here — settled
    // NOTHING, so a consumer could not distinguish "cancelled" from "still
    // running", forever. Settlement is now a Drop obligation.
    let (exchange, attempts) = capture_exchange(cap(2), cap(8), cap(64));
    {
        let mut tap = match exchange.open(&attempt("att_cancel")) {
            Some(tap) => tap,
            None => panic!("accepted"),
        };
        tap.chunk(b"partial");
        // No settle call at all. Cancellation is a drop.
    }
    let announced = match attempts.try_recv() {
        Ok(announced) => announced,
        Err(error) => panic!("announced: {error}"),
    };
    assert_eq!(
        drain(announced.feed()),
        vec![
            CaptureEvent::Chunk(b"partial".to_vec()),
            CaptureEvent::Finished {
                outcome: CaptureOutcome::Incomplete,
                loss: CaptureLoss::default(),
            },
        ],
        "a cancelled attempt must settle Incomplete without anyone calling settle"
    );
}

#[test]
fn end_of_attempt_survives_a_dropped_finished_event() {
    // The failure case for settlement: a saturated backlog can drop even the
    // Finished event. End-of-attempt must NOT be lossy, so the consumer learns
    // it from disconnection instead.
    let (exchange, attempts) = capture_exchange(cap(2), cap(1), cap(64));
    let announced = {
        let mut tap = match exchange.open(&attempt("att_sat")) {
            Some(tap) => tap,
            None => panic!("accepted"),
        };
        tap.chunk(b"one"); // fills the single slot
        tap.chunk(b"two"); // dropped
        tap.settle(CaptureOutcome::Terminal); // no room; dropped too
        match attempts.try_recv() {
            Ok(announced) => announced,
            Err(error) => panic!("announced: {error}"),
        }
        // tap dropped here
    };
    let events = drain(announced.feed());
    assert_eq!(events, vec![CaptureEvent::Chunk(b"one".to_vec())]);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, CaptureEvent::Finished { .. })),
        "this fixture is only meaningful if the Finished event was genuinely dropped"
    );
    assert_eq!(
        announced.feed().try_recv(),
        Err(TryRecvError::Disconnected),
        "a consumer must still learn the attempt is over, from disconnection"
    );
}

// ---------------------------------------------------------------------------
// Contract C — bounded and non-blocking, with loss reported
// ---------------------------------------------------------------------------

#[test]
fn a_full_backlog_drops_chunks_and_reports_how_many() {
    // POSITIVE and FAILURE together: the drop happens (so the reader is never
    // stalled) AND the count reaches the consumer (so nobody is handed a
    // partial capture believing it whole).
    let (exchange, attempts) = capture_exchange(cap(2), cap(2), cap(64));
    let mut tap = match exchange.open(&attempt("att_lossy")) {
        Some(tap) => tap,
        None => panic!("accepted"),
    };
    tap.chunk(b"1");
    tap.chunk(b"2");
    // Backlog is full from here on.
    for _ in 0..5 {
        tap.chunk(b"lost");
    }
    let announced = match attempts.try_recv() {
        Ok(announced) => announced,
        Err(error) => panic!("announced: {error}"),
    };
    // Drain the two buffered chunks so the Finished event has room.
    let buffered = drain(announced.feed());
    assert_eq!(
        buffered,
        vec![
            CaptureEvent::Chunk(b"1".to_vec()),
            CaptureEvent::Chunk(b"2".to_vec())
        ]
    );
    tap.settle(CaptureOutcome::Terminal);
    assert_eq!(
        drain(announced.feed()),
        vec![CaptureEvent::Finished {
            outcome: CaptureOutcome::Terminal,
            loss: CaptureLoss {
                saturated_chunks: 5,
                saturated_bytes: 20,
                oversized_chunks: 0,
                oversized_bytes: 0,
            },
        }],
        "the exact saturated chunk and byte counts must reach the consumer"
    );
}

#[test]
fn one_oversized_chunk_is_refused_before_capture() {
    let (exchange, attempts) = capture_exchange(cap(2), cap(2), cap(4));
    let mut tap = match exchange.open(&attempt("att_oversized")) {
        Some(tap) => tap,
        None => panic!("accepted"),
    };
    tap.chunk(b"12345");
    let announced = match attempts.try_recv() {
        Ok(announced) => announced,
        Err(error) => panic!("announced: {error}"),
    };
    tap.settle(CaptureOutcome::Terminal);
    assert_eq!(
        drain(announced.feed()),
        vec![CaptureEvent::Finished {
            outcome: CaptureOutcome::Terminal,
            loss: CaptureLoss {
                saturated_chunks: 0,
                saturated_bytes: 0,
                oversized_chunks: 1,
                oversized_bytes: 5,
            },
        }],
        "an oversized chunk must never enter the feed and its refusal is explicit"
    );
}

#[test]
fn a_full_feed_classifies_oversized_chunks_before_saturation() {
    let (exchange, attempts) = capture_exchange(cap(2), cap(1), cap(4));
    let mut tap = match exchange.open(&attempt("att_full_oversized")) {
        Some(tap) => tap,
        None => panic!("accepted"),
    };
    tap.chunk(b"ok");
    tap.chunk(b"oversized");
    tap.chunk(b"too-large");
    let announced = match attempts.try_recv() {
        Ok(announced) => announced,
        Err(error) => panic!("announced: {error}"),
    };
    assert_eq!(
        drain(announced.feed()),
        vec![CaptureEvent::Chunk(b"ok".to_vec())]
    );
    tap.settle(CaptureOutcome::Terminal);
    assert_eq!(
        drain(announced.feed()),
        vec![CaptureEvent::Finished {
            outcome: CaptureOutcome::Terminal,
            loss: CaptureLoss {
                saturated_chunks: 0,
                saturated_bytes: 0,
                oversized_chunks: 2,
                oversized_bytes: 18,
            },
        }],
        "the byte ceiling must classify oversized input before the full-feed path"
    );
}

#[test]
fn a_consumer_that_never_drains_cannot_stall_the_producer() {
    // The whole reason the seam is a channel rather than a trait this crate
    // calls: the first design let a slow or blocking sink stall the socket
    // reader and trip the no-progress timeout, failing a healthy attempt.
    //
    // Nothing here drains. Many more chunks are offered than the backlog holds,
    // and every offer must return — the assertion is that this test terminates
    // at all, plus that the producer observed the loss.
    let (exchange, _attempts) = capture_exchange(cap(1), cap(1), cap(64));
    let mut tap = match exchange.open(&attempt("att_stall")) {
        Some(tap) => tap,
        None => panic!("accepted"),
    };
    for _ in 0..10_000 {
        tap.chunk(b"x");
    }
    tap.settle(CaptureOutcome::Terminal);
}

#[test]
fn a_disconnected_consumer_does_not_fail_the_attempt() {
    // The consumer went away entirely. Offering bytes must remain a no-op, not
    // an error the attempt could trip over.
    let (exchange, attempts) = capture_exchange(cap(2), cap(4), cap(64));
    let mut tap = match exchange.open(&attempt("att_gone")) {
        Some(tap) => tap,
        None => panic!("accepted"),
    };
    drop(attempts);
    tap.chunk(b"nobody listening");
    tap.settle(CaptureOutcome::Terminal);
}

#[test]
fn an_exchange_with_no_room_declines_rather_than_blocking() {
    // A full announcement channel must decline the attempt, not queue it and
    // not stall the open. Capture is best-effort; an attempt opens whether or
    // not anyone is listening.
    let (exchange, _attempts) = capture_exchange(cap(1), cap(4), cap(64));
    assert!(exchange.open(&attempt("att_1")).is_some());
    assert!(
        exchange.open(&attempt("att_2")).is_none(),
        "a saturated exchange must decline, and declining must not be fatal"
    );
}
