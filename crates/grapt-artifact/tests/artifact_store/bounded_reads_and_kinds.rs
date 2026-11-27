// ------------------------------------------------------------------ budgeting

#[test]
fn the_declared_kind_is_part_of_the_identity() {
    // H1. Identical bytes, identical disposition, identical length, identical
    // bytes-digest — differing ONLY in the declared kind. Cold review reproduced
    // the first revision collapsing these to one identity, returning Ok with no
    // signal, and then reporting the plan's kind as CapturedOutput.
    let scratch = Scratch::new("kind-identity");
    let store = store(&scratch, 1);

    let mut seen = Vec::new();
    for kind in [
        ArtifactKind::CapturedOutput,
        ArtifactKind::CampaignPlan,
        ArtifactKind::Diagnostic,
    ] {
        let captured = match store.capture(
            b"identical bytes".as_slice(),
            bound(4096, 64),
            kind,
            Sensitivity::Ordinary,
        ) {
            Ok(captured) => captured,
            Err(error) => panic!("capture must succeed: {error}"),
        };
        let meta = match store.stat(captured.reference().id()) {
            Ok(meta) => meta,
            Err(error) => panic!("stat must succeed: {error}"),
        };
        assert_eq!(
            meta.kind(),
            kind,
            "a capture must report back the kind its caller declared"
        );
        seen.push(captured.reference().id().clone());
    }

    // The bytes-digest is shared — that is the point — so only the identity may
    // differ. Asserting the digests match first stops this passing because the
    // fixture accidentally used different content.
    assert_eq!(seen.len(), 3);
    assert_ne!(seen[0], seen[1], "CapturedOutput and CampaignPlan collided");
    assert_ne!(seen[1], seen[2], "CampaignPlan and Diagnostic collided");
    assert_ne!(seen[0], seen[2], "CapturedOutput and Diagnostic collided");
}

#[test]
fn an_empty_capture_still_separates_its_kinds() {
    // The collision cold review called reachable without contrivance: an empty
    // CapturedOutput and an empty Diagnostic are both ordinary outcomes and have
    // byte-identical content, so they collide immediately if kind is not bound.
    let scratch = Scratch::new("empty-kind");
    let store = store(&scratch, 1);

    let output = match store.capture(
        b"".as_slice(),
        bound(4096, 64),
        ArtifactKind::CapturedOutput,
        Sensitivity::Ordinary,
    ) {
        Ok(captured) => captured,
        Err(error) => panic!("an empty capture must succeed: {error}"),
    };
    let diagnostic = match store.capture(
        b"".as_slice(),
        bound(4096, 64),
        ArtifactKind::Diagnostic,
        Sensitivity::Ordinary,
    ) {
        Ok(captured) => captured,
        Err(error) => panic!("an empty capture must succeed: {error}"),
    };

    assert_eq!(output.reference().length(), 0);
    assert_eq!(diagnostic.reference().length(), 0);
    assert_eq!(
        output.reference().digest(),
        diagnostic.reference().digest(),
        "both are empty, so the bytes-digest must be shared"
    );
    assert_ne!(
        output.reference().id(),
        diagnostic.reference().id(),
        "two empty captures of different kinds must not share an identity"
    );

    let meta = match store.stat(diagnostic.reference().id()) {
        Ok(meta) => meta,
        Err(error) => panic!("stat must succeed: {error}"),
    };
    assert_eq!(meta.kind(), ArtifactKind::Diagnostic);
}

#[test]
fn a_budget_too_small_for_one_page_is_a_typed_refusal() {
    // The budget bounds the WHOLE response, not the payload. A budget under the
    // fixed overhead must produce a typed refusal naming the minimum — never a
    // panic, and never a silently oversized first item.
    // The budget now bounds PAYLOAD only — the fabricated 256-byte response
    // overhead is gone, by the L2 ruling. So the refusal that remains is
    // the honest one: a paged read cannot return less than a chunk, because a
    // partial chunk cannot be verified against its own digest.
    let scratch = Scratch::new("budget");
    let store = store(&scratch, 1);
    let body = vec![4_u8; 160 * 1024];
    let captured = match store.capture(
        body.as_slice(),
        bound(256 * 1024, 4096),
        ArtifactKind::CapturedOutput,
        Sensitivity::Ordinary,
    ) {
        Ok(captured) => captured,
        Err(error) => panic!("capture must succeed: {error}"),
    };

    match store.fetch(captured.reference(), 0, budget(1024)) {
        Err(FetchError::BudgetTooSmall { minimum }) => assert_eq!(
            minimum,
            64 * 1024,
            "the refusal must name one chunk as the minimum"
        ),
        other => panic!("a sub-chunk budget must be a typed refusal, got {other:?}"),
    }
}

#[test]
fn a_page_never_returns_more_payload_than_the_budget_allows() {
    // The positive half of the budget law. Without it, the refusal above could
    // be satisfied by a store that ignores the budget entirely on the paths that
    // do not refuse.
    let scratch = Scratch::new("budget-respected");
    let store = store(&scratch, 1);
    let body = vec![5_u8; 160 * 1024];
    let captured = match store.capture(
        body.as_slice(),
        bound(256 * 1024, 4096),
        ArtifactKind::CapturedOutput,
        Sensitivity::Ordinary,
    ) {
        Ok(captured) => captured,
        Err(error) => panic!("capture must succeed: {error}"),
    };

    // A budget under the whole artifact but over one chunk must page, not
    // return everything.
    let page = match store.fetch(captured.reference(), 0, budget(100 * 1024)) {
        Ok(page) => page,
        Err(error) => panic!("a one-chunk budget must page: {error}"),
    };
    assert!(
        page.bytes().len() as u64 <= 100 * 1024,
        "a page returned {} bytes against a 102400-byte budget",
        page.bytes().len()
    );
    assert_eq!(page.bytes().len(), 64 * 1024, "a page is one chunk");
    assert_eq!(page.next_offset(), Some(64 * 1024));
}

// ------------------------------------------------------- paging and per-chunk

#[test]
fn a_large_artifact_pages_and_each_page_verifies_against_its_own_chunk() {
    // Verifying only the whole digest on the final page would hand back every
    // earlier page unverified while looking sealed. Each page carries its own
    // chunk verification instead.
    let scratch = Scratch::new("paging");
    let store = store(&scratch, 1);

    // A bound of 256 KiB gives a 64 KiB chunk, so 160 KiB is three pages.
    let body = vec![9_u8; 160 * 1024];
    let captured = match store.capture(
        body.as_slice(),
        bound(256 * 1024, 4096),
        ArtifactKind::CapturedOutput,
        Sensitivity::Ordinary,
    ) {
        Ok(captured) => captured,
        Err(error) => panic!("capture must succeed: {error}"),
    };

    let page_budget = budget(64 * 1024 + 256);
    let mut offset = Some(0_u64);
    let mut seen = Vec::new();
    let mut pages = 0;
    while let Some(at) = offset {
        let page = match store.fetch(captured.reference(), at, page_budget) {
            Ok(page) => page,
            Err(error) => panic!("page at {at} must fetch: {error}"),
        };
        assert_eq!(
            page.verified(),
            Verified::Chunk {
                index: at / (64 * 1024)
            },
            "a page must report the chunk it was verified against, not Whole"
        );
        seen.extend_from_slice(page.bytes());
        offset = page.next_offset();
        pages += 1;
    }
    assert_eq!(pages, 3, "160 KiB in 64 KiB chunks is three pages");
    assert_eq!(seen, body, "paging must reassemble the exact artifact");
}

#[test]
fn a_corrupted_later_page_is_refused_even_though_earlier_pages_verified() {
    // The specific failure per-chunk digests exist to catch: page 0 is fine, so
    // a whole-digest-on-the-last-page design would have already handed the
    // caller two good pages before noticing.
    let scratch = Scratch::new("bad-page");
    let store = store(&scratch, 1);
    let body = vec![3_u8; 160 * 1024];
    let captured = match store.capture(
        body.as_slice(),
        bound(256 * 1024, 4096),
        ArtifactKind::CapturedOutput,
        Sensitivity::Ordinary,
    ) {
        Ok(captured) => captured,
        Err(error) => panic!("capture must succeed: {error}"),
    };

    let object = find_object(&scratch.0, captured.reference().id().as_str());
    let mut bytes = match std::fs::read(&object) {
        Ok(bytes) => bytes,
        Err(error) => panic!("the test must read the object: {error}"),
    };
    // Corrupt one byte inside the SECOND chunk, leaving the length unchanged.
    if let Some(slot) = bytes.get_mut(64 * 1024 + 10) {
        *slot = 0xFF;
    }
    match std::fs::write(&object, &bytes) {
        Ok(()) => {}
        Err(error) => panic!("the test must corrupt the object: {error}"),
    }

    let page_budget = budget(64 * 1024 + 256);
    let first = store.fetch(captured.reference(), 0, page_budget);
    assert!(first.is_ok(), "the untouched first page must still verify");

    assert_eq!(
        store
            .fetch(captured.reference(), 64 * 1024, page_budget)
            .err(),
        Some(FetchError::Unavailable),
        "the corrupted page must refuse on its own chunk digest"
    );
}

#[test]
fn an_unaligned_offset_is_refused_because_it_could_not_be_verified() {
    let scratch = Scratch::new("unaligned");
    let store = store(&scratch, 1);
    let body = vec![1_u8; 160 * 1024];
    let captured = match store.capture(
        body.as_slice(),
        bound(256 * 1024, 4096),
        ArtifactKind::CapturedOutput,
        Sensitivity::Ordinary,
    ) {
        Ok(captured) => captured,
        Err(error) => panic!("capture must succeed: {error}"),
    };
    match store.fetch(captured.reference(), 7, budget(64 * 1024 + 256)) {
        Err(FetchError::UnalignedOffset { chunk_size }) => assert_eq!(chunk_size, 64 * 1024),
        other => panic!("an unaligned offset must be a typed refusal, got {other:?}"),
    }
}

// ----------------------------------------------------------------- truncation

#[test]
fn a_truncated_capture_stores_exactly_the_bound_and_says_so() {
    let scratch = Scratch::new("truncated");
    let store = store(&scratch, 1);
    let captured = capture(&store, b"0123456789", 4);

    assert_eq!(
        captured.disposition(),
        CaptureDisposition::TruncatedAtBoundary { captured: 4 }
    );
    assert!(!captured.is_complete());
    assert_eq!(captured.reference().length(), 4);

    let page = match store.fetch(captured.reference(), 0, budget(64 * 1024)) {
        Ok(page) => page,
        Err(error) => panic!("a truncated artifact still fetches: {error}"),
    };
    assert_eq!(page.bytes(), b"0123", "no byte beyond the bound is stored");
}

// ------------------------------------------------------------------- campaign

#[test]
fn campaign_plan_bytes_are_stored_byte_identically() {
    // F2B receives ALREADY-CANONICAL bytes. The canonical encoding and the graph
    // digest belong to grapt-schema. If this crate ever normalised, re-encoded
    // or re-ordered what it was handed, the stored bytes would stop matching the
    // digest the authority approved.
    let scratch = Scratch::new("campaign");
    let store = store(&scratch, 1);

    // Deliberately contains whitespace, ordering and duplicate-looking keys that
    // a "helpful" canonicaliser would rewrite.
    let canonical = br#"{"b":1,"a":2,  "nested":{"z":[3,2,1]}}"#;
    let captured = match store.capture(
        canonical.as_slice(),
        bound(4096, 64),
        ArtifactKind::CampaignPlan,
        Sensitivity::Ordinary,
    ) {
        Ok(captured) => captured,
        Err(error) => panic!("capture must succeed: {error}"),
    };

    let page = match store.fetch(captured.reference(), 0, budget(64 * 1024)) {
        Ok(page) => page,
        Err(error) => panic!("fetch must succeed: {error}"),
    };
    assert_eq!(
        page.bytes(),
        canonical.as_slice(),
        "a plan artifact must round-trip byte-identically, whitespace and order included"
    );

    let meta = match store.stat(captured.reference().id()) {
        Ok(meta) => meta,
        Err(error) => panic!("stat must succeed: {error}"),
    };
    assert_eq!(meta.kind(), ArtifactKind::CampaignPlan);
}
