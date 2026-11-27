// ---------------------------------------------------------------- round trip

#[test]
fn a_captured_artifact_round_trips_and_reports_whole_verification() {
    let scratch = Scratch::new("round-trip");
    let store = store(&scratch, 1_700_000_000_000);
    let captured = capture(&store, b"hello artifact", 4096);

    assert!(captured.is_complete());
    assert_eq!(captured.reference().length(), 14);

    let page = match store.fetch(captured.reference(), 0, budget(64 * 1024)) {
        Ok(page) => page,
        Err(error) => panic!("fetching a good reference must succeed: {error}"),
    };
    assert_eq!(page.bytes(), b"hello artifact");
    assert_eq!(page.verified(), Verified::Whole);
    assert_eq!(page.next_offset(), None);
}

#[test]
fn the_capture_receipt_time_comes_from_the_injected_clock() {
    // Row 11: durable owners consume an injected clock. If this crate ever read
    // ambient time, this value would not be reproducible.
    let scratch = Scratch::new("clock");
    let store = store(&scratch, 424_242);
    let captured = capture(&store, b"timed", 4096);

    let meta = match store.stat(captured.reference().id()) {
        Ok(meta) => meta,
        Err(error) => panic!("stat must succeed: {error}"),
    };
    assert_eq!(meta.captured_at().as_millis(), 424_242);
}

// ------------------------------------------- the reference decision reads none

#[test]
fn the_reference_decision_reads_zero_payload_bytes() {
    // THE load-bearing law: "a reference path that materializes the payload
    // before deciding to return the reference" is not bounded.
    //
    // Proven by removing the object file entirely. Length, digest and
    // disposition still answer, because they come from recorded metadata. If
    // `stat` touched the payload this could not possibly succeed — the file is
    // gone. Asserting on a returned value instead would pass for the wrong
    // reason, since a store that read the payload would return the same numbers.
    let scratch = Scratch::new("no-payload");
    let store = store(&scratch, 1);
    let captured = capture(&store, b"a payload nobody needs to read", 4096);
    let id = captured.reference().id().clone();

    let object = find_object(&scratch.0, id.as_str());
    match std::fs::remove_file(&object) {
        Ok(()) => {}
        Err(error) => panic!("the test must be able to remove the object: {error}"),
    }

    let meta = match store.stat(&id) {
        Ok(meta) => meta,
        Err(error) => panic!("stat must not need the payload: {error}"),
    };
    assert_eq!(meta.length(), 30);
    assert!(meta.disposition().is_complete());

    // And the payload really was required for content: a fetch now fails.
    assert_eq!(
        store
            .fetch(captured.reference(), 0, budget(64 * 1024))
            .err(),
        Some(FetchError::Unavailable),
        "the object is gone, so content must be unavailable"
    );
}

fn find_object(root: &std::path::Path, id: &str) -> PathBuf {
    let prefix = id.get(..2).unwrap_or("__");
    root.join("objects").join(prefix).join(id)
}

fn find_meta(root: &std::path::Path, id: &str) -> PathBuf {
    let prefix = id.get(..2).unwrap_or("__");
    root.join("meta").join(prefix).join(id)
}

// ------------------------------------------------------- verification refusals

#[test]
fn a_reference_claiming_a_wrong_length_is_refused() {
    // Separated from the digest witness on purpose: the digest here is CORRECT,
    // so only the length comparison can fire. If both were wrong, one witness
    // would cover two laws and attribute neither.
    let scratch = Scratch::new("wrong-length");
    let store = store(&scratch, 1);
    let captured = capture(&store, b"exact bytes", 4096);
    let real = captured.reference();

    let lying = ArtifactRef::new(real.id().clone(), real.length() + 1, real.digest().clone());
    assert_eq!(
        store.fetch(&lying, 0, budget(64 * 1024)).err(),
        Some(FetchError::Unavailable),
        "a reference whose claimed length disagrees with the record must refuse"
    );
}

#[test]
fn a_reference_claiming_a_wrong_digest_is_refused() {
    // Length is CORRECT here, so only the digest comparison can fire.
    let scratch = Scratch::new("wrong-digest");
    let store = store(&scratch, 1);
    let captured = capture(&store, b"exact bytes", 4096);
    let real = captured.reference();

    let other = match grapt_types::ArtifactDigest::new(
        "0000000000000000000000000000000000000000000000000000000000000000",
    ) {
        Some(digest) => digest,
        None => panic!("a 64-character hex digest is never empty"),
    };
    let lying = ArtifactRef::new(real.id().clone(), real.length(), other);
    assert_eq!(
        store.fetch(&lying, 0, budget(64 * 1024)).err(),
        Some(FetchError::Unavailable),
        "a reference whose claimed digest disagrees with the record must refuse"
    );
}

#[test]
fn a_corrupt_object_is_refused_rather_than_returned() {
    // The reference is entirely truthful; only the stored bytes rotted. This is
    // what makes fetch verification, rather than construction, the seal.
    let scratch = Scratch::new("corrupt");
    let store = store(&scratch, 1);
    let captured = capture(&store, b"original content", 4096);
    let object = find_object(&scratch.0, captured.reference().id().as_str());

    // Same LENGTH, different bytes: the length check cannot fire, so only the
    // digest comparison separates the two sides.
    match std::fs::write(&object, b"corrupted conten") {
        Ok(()) => {}
        Err(error) => panic!("the test must be able to corrupt the object: {error}"),
    }

    assert_eq!(
        store
            .fetch(captured.reference(), 0, budget(64 * 1024))
            .err(),
        Some(FetchError::Unavailable),
        "corrupt bytes must never be returned as verified content"
    );
}

#[test]
fn a_record_whose_disposition_was_altered_no_longer_reproduces_its_identity() {
    // The identity is a digest over (disposition, length, bytes-digest), so
    // disposition is pinned by the reference even though F1A's ArtifactRef does
    // not carry it. Flipping the recorded disposition leaves length and digest
    // correct, so ONLY the identity reproduction can fire.
    let scratch = Scratch::new("disposition");
    let store = store(&scratch, 1);
    let captured = capture(&store, b"complete value", 4096);
    let meta_path = find_meta(&scratch.0, captured.reference().id().as_str());

    let text = match std::fs::read_to_string(&meta_path) {
        Ok(text) => text,
        Err(error) => panic!("the test must read the record: {error}"),
    };
    let tampered = text.replace("disposition complete", "disposition truncated");
    assert_ne!(text, tampered, "the fixture must really alter the record");
    match std::fs::write(&meta_path, tampered) {
        Ok(()) => {}
        Err(error) => panic!("the test must rewrite the record: {error}"),
    }

    assert_eq!(
        store
            .fetch(captured.reference(), 0, budget(64 * 1024))
            .err(),
        Some(FetchError::Unavailable),
        "a record claiming a different disposition must not satisfy the reference"
    );
}

#[test]
fn identical_bytes_with_different_dispositions_do_not_collide() {
    // A complete capture of four bytes and a capture truncated at four bytes of
    // a longer source store IDENTICAL bytes. A pure content digest would give
    // them one identity and one truth.
    let scratch = Scratch::new("collision");
    let store = store(&scratch, 1);

    let complete = capture(&store, b"abcd", 4);
    let truncated = capture(&store, b"abcdEXTRA", 4);

    assert_eq!(
        complete.reference().length(),
        truncated.reference().length()
    );
    assert_eq!(
        complete.reference().digest(),
        truncated.reference().digest(),
        "the stored bytes really are identical, so the bytes-digest must match"
    );
    assert!(complete.is_complete());
    assert_eq!(
        truncated.disposition(),
        CaptureDisposition::TruncatedAtBoundary { captured: 4 }
    );
    assert_ne!(
        complete.reference().id(),
        truncated.reference().id(),
        "identical bytes with different dispositions must not share an identity"
    );
}

#[test]
fn the_same_bytes_and_disposition_are_stored_once() {
    let scratch = Scratch::new("dedup");
    let store = store(&scratch, 1);
    let first = capture(&store, b"repeated", 4096);
    let second = capture(&store, b"repeated", 4096);
    assert_eq!(first.reference().id(), second.reference().id());
}
