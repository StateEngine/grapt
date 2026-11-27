use super::*;

/// A block whose deltas exceed the named bound is captured TRUNCATED AT THE
/// BOUNDARY, not silently trimmed and not held as an unbounded string.
#[test]
fn an_oversized_block_captures_truncated_rather_than_growing_without_bound() {
    let scratch = Scratch::new("oversize");
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let mut owner = match DurableAttemptOwner::elect(
        Arc::clone(&store),
        scratch.artifacts(),
        &clock,
        &ids,
        RecordingSink::default(),
        session(),
        step(1),
        None,
    ) {
        Ok(owner) => owner,
        Err(error) => panic!("the owner must elect: {error}"),
    };
    // One delta past the bound, then another entirely past it.
    let oversize = "x".repeat(MAX_BLOCK_CAPTURE_BYTES as usize + 64);
    drive(
        &mut owner,
        vec![
            DurableFact::AttemptStarted {
                attempt: attempt("pat_1"),
            },
            text_opened(0),
            text_delta(0, &oversize),
            text_delta(0, "more"),
            text_closed(0),
        ],
    );
    let history = history(&store);
    let messages = match history.messages() {
        Ok(messages) => messages,
        Err(error) => panic!("the folded history must render: {error}"),
    };
    let content = match &messages[0].content {
        Some(content) => content,
        None => panic!("the block produced an item"),
    };
    // The captured payload stopped AT the bound.
    assert_eq!(
        content.items()[0].item.payload().length(),
        MAX_BLOCK_CAPTURE_BYTES
    );
    // AND the durable disposition says bytes were LOST. This is the capture
    // truth: the block produced more than the bound, so the stored artifact is
    // a prefix and no reader may present it as the whole value. An earlier
    // revision asserted `Complete` here on the theory that a bounded producer
    // hands the capture exactly its bound -- true, and precisely why the
    // capture boundary cannot see the truncation. Asserting the boundary's
    // blindness as the law made a truncated transcript durably whole.
    assert_eq!(
        content.items()[0].disposition,
        CaptureDisposition::TruncatedAtBoundary {
            captured: MAX_BLOCK_CAPTURE_BYTES
        },
        "bytes were dropped past the bound, so the durable fact must say the \
         stored artifact is a bounded PREFIX and name how much was kept"
    );
    assert!(
        !content.is_complete(),
        "a message holding a truncated item is not a complete transcript"
    );
    // And working memory was bounded INDEPENDENTLY of that disposition. This is
    // the separation the length assertion could not make and the disposition
    // assertion above no longer makes either: an unbounded accumulator would
    // also yield a MAX-length artifact and a truncated disposition, and only
    // the high-water mark tells the two apart. `IGSD07` fires on this line.
    assert!(
        owner.peak_block_bytes() <= MAX_BLOCK_CAPTURE_BYTES,
        "one block held {} bytes in working memory; the bound is {}",
        owner.peak_block_bytes(),
        MAX_BLOCK_CAPTURE_BYTES
    );
}

/// A block at the product's 256 KiB public bound, arriving as many
/// coalescing-sized deltas, is captured WHOLE. The capture bound is its own
/// literal: a capture tied to the (deliberately small) coalesced-delta
/// ceiling would truncate ordinary long output.
#[test]
fn a_block_at_the_public_bound_is_captured_whole() {
    let scratch = Scratch::new("public-bound");
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let mut owner = match DurableAttemptOwner::elect(
        Arc::clone(&store),
        scratch.artifacts(),
        &clock,
        &ids,
        RecordingSink::default(),
        session(),
        step(1),
        None,
    ) {
        Ok(owner) => owner,
        Err(error) => panic!("the owner must elect: {error}"),
    };
    let chunk = "x".repeat(AttemptFragment::MAX_COALESCED_DELTA_BYTES);
    let mut facts = vec![
        DurableFact::AttemptStarted {
            attempt: attempt("pat_1"),
        },
        text_opened(0),
    ];
    let chunks = (256 * 1024) / chunk.len();
    facts.extend((0..chunks).map(|_| text_delta(0, &chunk)));
    facts.push(text_closed(0));
    drive(&mut owner, facts);
    let history = history(&store);
    let messages = match history.messages() {
        Ok(messages) => messages,
        Err(error) => panic!("the folded history must render: {error}"),
    };
    let content = match &messages[0].content {
        Some(content) => content,
        None => panic!("the block produced an item"),
    };
    assert_eq!(content.items()[0].item.payload().length(), 256 * 1024);
    assert_eq!(content.items()[0].disposition, CaptureDisposition::Complete);
}
