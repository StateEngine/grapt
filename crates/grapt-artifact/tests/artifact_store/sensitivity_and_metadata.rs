// -------------------------------------------------------------------- secrets

#[test]
fn a_secret_bearing_capture_is_not_a_public_offline_equality_oracle() {
    // The secret law forbids "a stable offline equality oracle". A bare SHA-256
    // of guessable material is exactly that: hash the guess and compare. So the
    // secret-bearing identity must NOT be the plain hash of its bytes.
    let scratch = Scratch::new("secret");
    let store = store(&scratch, 1);

    let ordinary = match store.capture(
        b"hunter2".as_slice(),
        bound(4096, 64),
        ArtifactKind::Diagnostic,
        Sensitivity::Ordinary,
    ) {
        Ok(captured) => captured,
        Err(error) => panic!("capture must succeed: {error}"),
    };
    let secret = match store.capture(
        b"hunter2".as_slice(),
        bound(4096, 64),
        ArtifactKind::Diagnostic,
        Sensitivity::SecretBearing,
    ) {
        Ok(captured) => captured,
        Err(error) => panic!("capture must succeed: {error}"),
    };

    assert_ne!(
        ordinary.reference().digest(),
        secret.reference().digest(),
        "a secret-bearing digest must not be the guessable plain hash of its bytes"
    );
    assert_ne!(ordinary.reference().id(), secret.reference().id());
}

#[test]
fn a_secret_bearing_artifact_round_trips_through_fetch() {
    // Keying lives in TWO places — the streaming capture digest and the
    // whole-value digest the fetch verifies with — and they must agree or every
    // secret-bearing fetch fails. The mutation run caught the gap: with no
    // witness that FETCHES secret-bearing content, the fetch half of the keying
    // could be deleted and nothing turned red.
    let scratch = Scratch::new("secret-fetch");
    let store = store(&scratch, 1);
    let secret = match store.capture(
        b"a secret worth verifying".as_slice(),
        bound(4096, 64),
        ArtifactKind::Diagnostic,
        Sensitivity::SecretBearing,
    ) {
        Ok(captured) => captured,
        Err(error) => panic!("capture must succeed: {error}"),
    };

    let page = match store.fetch(secret.reference(), 0, budget(64 * 1024)) {
        Ok(page) => page,
        Err(error) => panic!("a secret-bearing artifact must verify on fetch: {error}"),
    };
    assert_eq!(page.bytes(), b"a secret worth verifying");
    assert_eq!(page.verified(), Verified::Whole);
}

#[test]
fn a_secret_bearing_artifact_pages_with_every_chunk_keyed() {
    // H3. Keying appears at FOUR sites: the whole digest and the FIRST chunk in
    // RollingDigest::new, every chunk after the first in seal_chunk, and the
    // fetch-side digest. The previous evidence grid had SecretBearing x paged
    // fetch EMPTY, so seal_chunk's keying — the only site reached for chunks two
    // and beyond — had no witness. Deleting it left every test green while
    // BARE SHA-256 per-chunk digests of secret-bearing content were persisted to
    // disk in plaintext hex: an offline equality oracle at chunk granularity,
    // the exact thing this crate exists to prevent.
    //
    // This artifact is 160 KiB against a 64 KiB chunk, so chunks 1 and 2 are
    // sealed by seal_chunk and must verify on fetch.
    let scratch = Scratch::new("secret-paged");
    let store = store(&scratch, 1);
    let body = vec![7_u8; 160 * 1024];
    let secret = match store.capture(
        body.as_slice(),
        bound(256 * 1024, 4096),
        ArtifactKind::Diagnostic,
        Sensitivity::SecretBearing,
    ) {
        Ok(captured) => captured,
        Err(error) => panic!("capture must succeed: {error}"),
    };

    let page_budget = budget(64 * 1024);
    let mut offset = Some(0_u64);
    let mut seen = Vec::new();
    let mut indices = Vec::new();
    while let Some(at) = offset {
        let page = match store.fetch(secret.reference(), at, page_budget) {
            Ok(page) => page,
            Err(error) => panic!("secret-bearing page at {at} must verify: {error}"),
        };
        match page.verified() {
            Verified::Chunk { index } => indices.push(index),
            other => panic!("a paged read must report a chunk, got {other:?}"),
        }
        seen.extend_from_slice(page.bytes());
        offset = page.next_offset();
    }
    assert_eq!(indices, vec![0, 1, 2], "all three chunks must verify");
    assert_eq!(seen, body, "paging must reassemble the exact artifact");

    // And the stored per-chunk digests must NOT be the bare hashes of those
    // chunks — that is the oracle the keying exists to deny.
    let bare_chunk = {
        use sha2::Digest as _;
        let mut hasher = sha2::Sha256::new();
        hasher.update(vec![7_u8; 64 * 1024]);
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let record =
        match std::fs::read_to_string(find_meta(&scratch.0, secret.reference().id().as_str())) {
            Ok(text) => text,
            Err(error) => panic!("the test must read the record: {error}"),
        };
    assert!(
        !record.contains(&bare_chunk),
        "a bare SHA-256 chunk digest of secret-bearing content reached disk"
    );
}

#[test]
fn an_oversized_metadata_record_is_refused_before_it_is_read() {
    // M1. `stat` is the one path documented as the bounded reference decision,
    // and corruption is inside this crate's threat model, so it must not be the
    // path that reads a file of any size. MAX_CHUNKS bounds what the WRITER
    // emits; nothing bounded what the reader ingested.
    let scratch = Scratch::new("huge-metadata");
    let store = store(&scratch, 1);
    let captured = capture(&store, b"small", 4096);
    let meta_path = find_meta(&scratch.0, captured.reference().id().as_str());

    // Built from the REAL record so identity, length and digest stay correct and
    // the chunk COUNT stays at one. Only the byte bound can refuse this: with
    // the bound removed it parses cleanly, verifies its identity and returns Ok.
    // A first draft of this witness padded with 40,000 chunk lines, which trips
    // the chunk-count cap instead — one test covering two laws, attributing
    // neither. The two are separated deliberately here.
    let text = match std::fs::read_to_string(&meta_path) {
        Ok(text) => text,
        Err(error) => panic!("the test must read the record: {error}"),
    };
    let padded = text.trim_end().to_owned() + &"0".repeat(400 * 1024) + "\n";
    assert!(
        padded.len() as u64 > 256 * 1024,
        "the fixture must actually exceed the bound"
    );
    match std::fs::write(&meta_path, &padded) {
        Ok(()) => {}
        Err(error) => panic!("the test must write the record: {error}"),
    }

    assert_eq!(
        store.stat(captured.reference().id()).err(),
        Some(StatError::Unavailable),
        "a record over the byte bound must be refused before it is ingested"
    );
}

#[test]
fn a_metadata_record_over_the_chunk_cap_is_refused() {
    // M1's other half. Under the byte bound, so only the parsed-count cap can
    // refuse it. MAX_CHUNKS bounds what the writer emits; this bounds what the
    // reader will accumulate from a corrupt record.
    let scratch = Scratch::new("chunk-cap");
    let store = store(&scratch, 1);
    let captured = capture(&store, b"small", 4096);
    let meta_path = find_meta(&scratch.0, captured.reference().id().as_str());

    let text = match std::fs::read_to_string(&meta_path) {
        Ok(text) => text,
        Err(error) => panic!("the test must read the record: {error}"),
    };
    let mut extended = text;
    for _ in 0..1100 {
        extended
            .push_str("chunk 0000000000000000000000000000000000000000000000000000000000000000\n");
    }
    assert!(
        extended.len() as u64 <= 256 * 1024,
        "the fixture must stay UNDER the byte bound so only the count cap fires"
    );
    match std::fs::write(&meta_path, &extended) {
        Ok(()) => {}
        Err(error) => panic!("the test must write the record: {error}"),
    }

    assert_eq!(
        store.stat(captured.reference().id()).err(),
        Some(StatError::Unavailable),
        "a record claiming more chunks than the cap must be refused"
    );
}

#[test]
fn a_record_that_does_not_vouch_for_its_own_identity_is_refused_by_stat() {
    // L3. Without this, corrupt metadata makes stat().kind() and .reference()
    // report a false typed fact on the ZERO-PAYLOAD path — the very path a
    // caller is told to trust without reading the bytes. Only the KIND line is
    // altered, so length and digest stay correct and only the identity
    // reproduction can fire.
    let scratch = Scratch::new("false-fact");
    let store = store(&scratch, 1);
    let captured = match store.capture(
        b"a plan".as_slice(),
        bound(4096, 64),
        ArtifactKind::CampaignPlan,
        Sensitivity::Ordinary,
    ) {
        Ok(captured) => captured,
        Err(error) => panic!("capture must succeed: {error}"),
    };
    let meta_path = find_meta(&scratch.0, captured.reference().id().as_str());
    let text = match std::fs::read_to_string(&meta_path) {
        Ok(text) => text,
        Err(error) => panic!("the test must read the record: {error}"),
    };
    let tampered = text.replace("kind campaign-plan", "kind captured-output");
    assert_ne!(text, tampered, "the fixture must really alter the record");
    match std::fs::write(&meta_path, tampered) {
        Ok(()) => {}
        Err(error) => panic!("the test must rewrite the record: {error}"),
    }

    assert_eq!(
        store.stat(captured.reference().id()).err(),
        Some(StatError::Unavailable),
        "stat must refuse a record whose kind no longer reproduces its identity"
    );
}

#[test]
fn a_secret_bearing_record_reveals_neither_payload_nor_length_in_debug() {
    // The law names "neither payload nor secret-dependent length". Because
    // `missing_debug_implementations` is warned and warnings are denied, every
    // type here MUST implement Debug — and a derive would print exactly this.
    let scratch = Scratch::new("redaction");
    let store = store(&scratch, 1);
    let secret = match store.capture(
        b"a-very-distinctive-secret".as_slice(),
        bound(4096, 64),
        ArtifactKind::Diagnostic,
        Sensitivity::SecretBearing,
    ) {
        Ok(captured) => captured,
        Err(error) => panic!("capture must succeed: {error}"),
    };
    let meta = match store.stat(secret.reference().id()) {
        Ok(meta) => meta,
        Err(error) => panic!("stat must succeed: {error}"),
    };

    let rendered = format!("{meta:?}");
    assert!(
        !rendered.contains("a-very-distinctive-secret"),
        "Debug must not print payload: {rendered}"
    );
    assert!(
        !rendered.contains("25"),
        "Debug must not print the secret-dependent length: {rendered}"
    );
    assert!(
        !rendered.contains(meta.digest().as_str()),
        "Debug must not print the digest of secret-bearing content: {rendered}"
    );
    assert!(
        rendered.contains("redacted"),
        "the redaction must be visible"
    );
}

#[test]
fn an_ordinary_record_still_reports_its_length_and_identity() {
    // The redaction must be scoped to secret-bearing content. Redacting
    // everything would be blind rather than safe.
    let scratch = Scratch::new("ordinary-debug");
    let store = store(&scratch, 1);
    let captured = capture(&store, b"public output", 4096);
    let meta = match store.stat(captured.reference().id()) {
        Ok(meta) => meta,
        Err(error) => panic!("stat must succeed: {error}"),
    };
    let rendered = format!("{meta:?}");
    assert!(
        rendered.contains("13"),
        "an ordinary length is not secret: {rendered}"
    );
}

// ------------------------------------------------------------------- absence

#[test]
fn an_unknown_identity_is_uniformly_unavailable() {
    // Uniform refusal: splitting "no such artifact" from "your claim is wrong"
    // would make stat and fetch an existence oracle over guessed identities.
    let scratch = Scratch::new("unknown");
    let store = store(&scratch, 1);
    let unknown = match grapt_types::ArtifactId::new("not-a-stored-identity") {
        Some(id) => id,
        None => panic!("a non-empty string is a valid opaque id"),
    };
    assert_eq!(store.stat(&unknown).err(), Some(StatError::Unavailable));
}
