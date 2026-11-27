use super::*;
use std::cell::Cell;

const EPOCH: &str = "e1";

fn entry(seq: u64, frame: &str) -> ReplayEntry {
    ReplayEntry {
        seq,
        location: None,
        frame: Bytes::from(frame.to_owned()),
    }
}

fn located(seq: u64, directory: &str) -> Result<ReplayEntry, Box<dyn std::error::Error>> {
    Ok(ReplayEntry {
        seq,
        location: Some(LocationRef::new(directory, None)?),
        frame: Bytes::from(format!("frame-{seq}")),
    })
}

fn connected() -> Bytes {
    Bytes::from_static(b"event: message\ndata: {}\n\n")
}

fn resume(ring: &mut ReplayRing, id: &str, current: u64) -> Result<Vec<Bytes>, ResumeRefusal> {
    ring.opening_batch(id, (EPOCH, current), &connected(), (1_024, 1 << 20), |_| {
        true
    })
}

#[test]
fn the_ring_evicts_at_its_exact_frame_bound() {
    let mut ring = ReplayRing::with_bounds(3, usize::MAX);
    for seq in 1..=3 {
        ring.push(entry(seq, "x"));
    }
    assert_eq!(ring.counts().evicted_frames, 0, "three fit exactly");
    assert!(resume(&mut ring, "e1-0", 3).is_ok(), "nothing evicted yet");
    ring.push(entry(4, "x"));
    assert_eq!(ring.counts().evicted_frames, 1);
    assert_eq!(resume(&mut ring, "e1-0", 4), Err(ResumeRefusal::Evicted));
    assert_eq!(resume(&mut ring, "e1-1", 4).map(|batch| batch.len()), Ok(4));
}

#[test]
fn the_ring_evicts_at_its_exact_byte_bound() {
    let charge = entry(1, "abcd").charge();
    let mut ring = ReplayRing::with_bounds(usize::MAX, charge * 2);
    ring.push(entry(1, "abcd"));
    ring.push(entry(2, "abcd"));
    assert_eq!(ring.counts().evicted_frames, 0, "two charges fit exactly");
    ring.push(entry(3, "abcd"));
    assert_eq!(ring.counts().evicted_frames, 1);
    assert_eq!(resume(&mut ring, "e1-0", 3), Err(ResumeRefusal::Evicted));
    assert!(resume(&mut ring, "e1-1", 3).is_ok());
}

#[test]
fn a_barrier_refuses_every_cursor_before_it_and_admits_after() {
    let mut ring = ReplayRing::default();
    ring.push(entry(1, "a"));
    ring.barrier(2);
    ring.push(entry(3, "b"));
    assert_eq!(resume(&mut ring, "e1-1", 3), Err(ResumeRefusal::Barrier));
    let batch = resume(&mut ring, "e1-2", 3).map(|batch| batch.len());
    assert_eq!(batch, Ok(2), "the echo and the frame after the barrier");
    assert_eq!(ring.counts().barriers, 1);
}

#[test]
fn malformed_foreign_and_future_cursors_are_refused() {
    let mut ring = ReplayRing::default();
    ring.push(entry(1, "a"));
    for id in [
        "",
        "e1",
        "e1-",
        "e1-x",
        "e1-+1",
        &"e".repeat(MAX_RESUME_ID_BYTES + 1),
    ] {
        assert_eq!(
            resume(&mut ring, id, 1),
            Err(ResumeRefusal::Malformed),
            "{id}"
        );
    }
    assert_eq!(resume(&mut ring, "e2-1", 1), Err(ResumeRefusal::Epoch));
    assert_eq!(resume(&mut ring, "e1-2", 1), Err(ResumeRefusal::Ahead));
    let counts = ring.counts();
    assert_eq!((counts.refused_malformed, counts.refused_epoch), (6, 1));
    assert_eq!((counts.refused_ahead, counts.resumed), (1, 0));
}

#[test]
fn the_certificate_echoes_the_cursor_before_the_replay() {
    let mut ring = ReplayRing::default();
    ring.push(entry(5, "five"));
    ring.push(entry(6, "six"));
    let batch = resume(&mut ring, "e1-5", 6);
    let expected = [
        [b"id: e1-5\n".as_slice(), connected().as_ref()].concat(),
        b"six".to_vec(),
    ];
    assert_eq!(
        batch.map(|frames| frames
            .iter()
            .map(|frame| frame.to_vec())
            .collect::<Vec<_>>()),
        Ok(expected.to_vec())
    );
    assert_eq!(
        (ring.counts().resumed, ring.counts().replayed_frames),
        (1, 1)
    );
}

#[test]
fn the_batch_refuses_at_its_frame_and_byte_bounds_without_reading_on() {
    let mut ring = ReplayRing::default();
    for seq in 1..=5 {
        ring.push(entry(seq, "0123456789"));
    }
    let consulted = Cell::new(0);
    let counting = |_: Option<&LocationRef>| {
        consulted.set(consulted.get() + 1);
        true
    };
    // The echo plus two frames is three; the third replayed frame overruns.
    let frames = ring.opening_batch("e1-0", (EPOCH, 5), &connected(), (3, usize::MAX), counting);
    assert_eq!(frames, Err(ResumeRefusal::OverBatch));
    assert_eq!(consulted.get(), 3, "stopped at the first overrun");
    let echo = 9 + connected().len();
    consulted.set(0);
    let bytes = ring.opening_batch(
        "e1-0",
        (EPOCH, 5),
        &connected(),
        (usize::MAX, echo + 20),
        counting,
    );
    assert_eq!(bytes, Err(ResumeRefusal::OverBatch));
    assert_eq!(
        consulted.get(),
        3,
        "two frames fit exactly; the third overruns"
    );
    let fits = ring.opening_batch("e1-3", (EPOCH, 5), &connected(), (3, echo + 20), |_| true);
    assert_eq!(
        fits.map(|batch| batch.len()),
        Ok(3),
        "exactly at both bounds"
    );
    assert_eq!(ring.counts().refused_over_batch, 2);
}

#[test]
fn the_replay_is_filtered_by_the_new_requests_interest() -> Result<(), Box<dyn std::error::Error>> {
    let mut ring = ReplayRing::default();
    ring.push(located(1, "/work/a")?);
    ring.push(located(2, "/work/b")?);
    ring.push(entry(3, "global"));
    let batch = ring.opening_batch(
        "e1-0",
        (EPOCH, 3),
        &connected(),
        (1_024, 1 << 20),
        |location| location.is_none_or(|location| location.directory() == "/work/b"),
    );
    let frames = batch.map(|frames| frames[1..].to_vec());
    assert_eq!(
        frames,
        Ok(vec![Bytes::from("frame-2"), Bytes::from("global")])
    );
    Ok(())
}

#[test]
fn counts_saturate_without_wrapping() {
    let mut ring = ReplayRing::with_bounds(1, usize::MAX);
    ring.counts
        .update(|counts| counts.evicted_frames = u64::MAX);
    ring.push(entry(1, "a"));
    ring.push(entry(2, "b"));
    assert_eq!(ring.counts().evicted_frames, u64::MAX);
}
