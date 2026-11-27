// -------------------------------------------------------- carry admission

// The admission-time observation: the reference's claims checked against
// the recorded metadata, the object's length, and - under the explicit
// digest budget - the object's actual bytes re-digested by a bounded
// stream. Each witness isolates ONE mechanism so a regression attributes
// to its own line.

/// The budget every witness in this section pays into: generous enough
/// that no witness in this file can trip it while its own mechanism is
/// under test.
const TEST_CARRY_DIGEST_BUDGET: u64 = 16 * 1024 * 1024;

fn test_budget() -> CarryDigestBudget {
    CarryDigestBudget::new(TEST_CARRY_DIGEST_BUDGET)
}

#[test]
fn a_complete_carry_observes_complete() {
    let scratch = Scratch::new("carry-complete");
    let store = store(&scratch, 1_700_000_000_000);
    let captured = capture(&store, b"carried bytes", 4096);
    let mut budget = test_budget();

    assert_eq!(
        store.observe_carried(captured.reference(), &mut budget),
        CarryObservation::Complete,
        "a carried artifact that stands must observe complete",
    );
}

#[test]
fn a_missing_object_observes_absent() {
    let scratch = Scratch::new("carry-absent-object");
    let store = store(&scratch, 1);
    let captured = capture(&store, b"the bytes fell off the carry", 4096);
    let mut budget = test_budget();
    let id = captured.reference().id().clone();

    let object = find_object(&scratch.0, id.as_str());
    match std::fs::remove_file(&object) {
        Ok(()) => {}
        Err(error) => panic!("the test must be able to remove the object: {error}"),
    }

    assert_eq!(
        store.observe_carried(captured.reference(), &mut budget),
        CarryObservation::Absent,
        "a missing object is absent, not corrupt and not unreadable",
    );
}

#[test]
fn a_missing_metadata_observes_absent() {
    let scratch = Scratch::new("carry-absent-meta");
    let store = store(&scratch, 1);
    let captured = capture(&store, b"the record fell off the carry", 4096);
    let mut budget = test_budget();
    let id = captured.reference().id().clone();

    let meta = find_meta(&scratch.0, id.as_str());
    match std::fs::remove_file(&meta) {
        Ok(()) => {}
        Err(error) => panic!("the test must be able to remove the metadata: {error}"),
    }

    assert_eq!(
        store.observe_carried(captured.reference(), &mut budget),
        CarryObservation::Absent,
        "a missing record is absent, not corrupt and not unreadable",
    );
}

#[test]
fn a_directory_where_the_metadata_stands_observes_unreadable() {
    let scratch = Scratch::new("carry-dir-meta");
    let store = store(&scratch, 1);
    let captured = capture(&store, b"the record became a directory", 4096);
    let mut budget = test_budget();
    let id = captured.reference().id().clone();

    let meta = find_meta(&scratch.0, id.as_str());
    match std::fs::remove_file(&meta) {
        Ok(()) => {}
        Err(error) => panic!("the test must be able to remove the metadata: {error}"),
    }
    match std::fs::create_dir(&meta) {
        Ok(()) => {}
        Err(error) => panic!("the test must be able to create the directory: {error}"),
    }

    assert_eq!(
        store.observe_carried(captured.reference(), &mut budget),
        CarryObservation::Unreadable,
        "a file that cannot stand as a record is unreadable, not absent",
    );
}

#[test]
fn an_overlarge_metadata_observes_unreadable() {
    let scratch = Scratch::new("carry-oversized-meta");
    let store = store(&scratch, 1);
    let captured = capture(&store, b"the record grew past its bound", 4096);
    let mut budget = test_budget();
    let id = captured.reference().id().clone();

    let meta = find_meta(&scratch.0, id.as_str());
    let oversized = vec![b'x'; (256 * 1024 + 1) as usize];
    match std::fs::write(&meta, &oversized) {
        Ok(()) => {}
        Err(error) => panic!("the test must be able to overwrite the metadata: {error}"),
    }

    assert_eq!(
        store.observe_carried(captured.reference(), &mut budget),
        CarryObservation::Unreadable,
        "an over-bounded record is unreadable, and the bound applies before allocation",
    );
}

#[test]
fn a_record_that_cannot_parse_observes_corrupt() {
    let scratch = Scratch::new("carry-garbage-meta");
    let store = store(&scratch, 1);
    let captured = capture(&store, b"the record no longer parses", 4096);
    let mut budget = test_budget();
    let id = captured.reference().id().clone();

    let meta = find_meta(&scratch.0, id.as_str());
    match std::fs::write(&meta, b"this is not a metadata record") {
        Ok(()) => {}
        Err(error) => panic!("the test must be able to overwrite the metadata: {error}"),
    }

    assert_eq!(
        store.observe_carried(captured.reference(), &mut budget),
        CarryObservation::Corrupt,
        "an unparsable record is corrupt",
    );
}

#[test]
fn a_record_that_does_not_vouch_for_its_id_observes_corrupt() {
    let scratch = Scratch::new("carry-forged-id");
    let store = store(&scratch, 1);
    let captured = capture(&store, b"the record names the wrong identity", 4096);
    let mut budget = test_budget();
    let reference = captured.reference().clone();
    let id = reference.id().clone();

    let meta = find_meta(&scratch.0, id.as_str());
    let original = match std::fs::read_to_string(&meta) {
        Ok(text) => text,
        Err(error) => panic!("the test must be able to read the metadata: {error}"),
    };
    let forged = forge_line(&original, "id ", first_hex(id.as_str()));
    match std::fs::write(&meta, forged) {
        Ok(()) => {}
        Err(error) => panic!("the test must be able to overwrite the metadata: {error}"),
    }

    assert_eq!(
        store.observe_carried(&reference, &mut budget),
        CarryObservation::Corrupt,
        "a record whose id is not the reference id cannot vouch for the carry",
    );
}

#[test]
fn a_record_that_does_not_recompute_its_identity_observes_corrupt() {
    let scratch = Scratch::new("carry-forged-digest");
    let store = store(&scratch, 1);
    let captured = capture(&store, b"the record disagrees with its own identity", 4096);
    let mut budget = test_budget();
    let reference = captured.reference().clone();
    let id = reference.id().clone();

    let meta = find_meta(&scratch.0, id.as_str());
    let original = match std::fs::read_to_string(&meta) {
        Ok(text) => text,
        Err(error) => panic!("the test must be able to read the metadata: {error}"),
    };
    let forged = forge_line(&original, "digest ", "0");
    match std::fs::write(&meta, forged) {
        Ok(()) => {}
        Err(error) => panic!("the test must be able to overwrite the metadata: {error}"),
    }

    assert_eq!(
        store.observe_carried(&reference, &mut budget),
        CarryObservation::Corrupt,
        "a record whose fields no longer recompute its id cannot vouch for the carry",
    );
}

#[test]
fn a_reference_mismatching_the_recorded_length_observes_corrupt() {
    let scratch = Scratch::new("carry-wrong-length");
    let store = store(&scratch, 1);
    let captured = capture(&store, b"the reference claims a different size", 4096);
    let mut budget = test_budget();
    let reference = captured.reference().clone();
    let liar = grapt_types::ArtifactRef::new(
        reference.id().clone(),
        reference.length() + 1,
        reference.digest().clone(),
    );

    assert_eq!(
        store.observe_carried(&liar, &mut budget),
        CarryObservation::Corrupt,
        "a reference length that the record does not carry is corrupt",
    );
}

#[test]
fn an_object_of_the_wrong_length_observes_corrupt() {
    let scratch = Scratch::new("carry-wrong-object");
    let store = store(&scratch, 1);
    let captured = capture(&store, b"the object grew beyond its record", 4096);
    let mut budget = test_budget();
    let id = captured.reference().id().clone();

    let object = find_object(&scratch.0, id.as_str());
    use std::io::Write as _;
    let mut handle = match std::fs::OpenOptions::new().append(true).open(&object) {
        Ok(handle) => handle,
        Err(error) => panic!("the test must be able to open the object: {error}"),
    };
    match handle.write_all(b"+") {
        Ok(()) => {}
        Err(error) => panic!("the test must be able to grow the object: {error}"),
    }

    assert_eq!(
        store.observe_carried(captured.reference(), &mut budget),
        CarryObservation::Corrupt,
        "an object that no longer stands the recorded length is corrupt",
    );
}

#[test]
fn a_same_length_object_flip_observes_corrupt() {
    let scratch = Scratch::new("carry-flipped-byte");
    let store = store(&scratch, 1);
    let captured = capture(&store, b"one byte of this object flips in place", 4096);
    let mut budget = test_budget();
    let id = captured.reference().id().clone();

    let object = find_object(&scratch.0, id.as_str());
    let mut bytes = match std::fs::read(&object) {
        Ok(bytes) => bytes,
        Err(error) => panic!("the test must be able to read the object: {error}"),
    };
    bytes[0] ^= 0x01;
    match std::fs::write(&object, &bytes) {
        Ok(()) => {}
        Err(error) => panic!("the test must be able to rewrite the object: {error}"),
    }

    assert_eq!(
        store.observe_carried(captured.reference(), &mut budget),
        CarryObservation::Corrupt,
        "a same-length byte flip must observe corrupt at admission, not pass and fail later",
    );
}

#[test]
fn an_overbudget_carry_observes_budget_exceeded() {
    let scratch = Scratch::new("carry-overbudget");
    let store = store(&scratch, 1);
    let captured = capture(&store, b"thirteen bytes", 4096);
    let mut budget = CarryDigestBudget::new(3);

    assert_eq!(
        store.observe_carried(captured.reference(), &mut budget),
        CarryObservation::DigestBudgetExceeded,
        "an object that cannot fit the digest budget is refused, not admitted",
    );
}
/// Rewrites the first `prefix`-headed line of a metadata record with a
/// forged 64-hex value that differs from the real one in its first
/// character, so the forgery stays parseable and only the named check can
/// fire.
fn forge_line(original: &str, prefix: &str, real_first: &str) -> String {
    let first = if real_first == "0" { "1" } else { "0" };
    let mut out = String::new();
    let mut done = false;
    for line in original.lines() {
        if !done && line.starts_with(prefix) {
            out.push_str(prefix);
            out.push_str(first);
            for _ in 1..64 {
                out.push('0');
            }
            done = true;
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if !done {
        panic!("the metadata record had no {prefix:?} line");
    }
    out
}

fn first_hex(value: &str) -> &str {
    value
        .get(..1)
        .unwrap_or_else(|| panic!("an identity has a first character: {value}"))
}
