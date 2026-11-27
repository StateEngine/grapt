//! Capture is bounded before it allocates, and again before it completes — and
//! the ways a capture can fail stay distinct from each other.
//!
//! The reader counts its own probes and reads. That counter is what makes
//! "refused WITHOUT reading" a witnessable claim: the outcome alone cannot tell
//! a document refused before the read from one refused after it, and those are
//! the two implementations the bound exists to distinguish.

use crate::support;

use std::io;

use grapt_config::{discover, DeterministicSourceReader, DiscoveryPlan, SourceOutcome, SourceSlot};
use support::source_support::{abs, custom_source_bounds, file_name, identity, source_bounds};

const CONFIG: &str = "config.jsonc";

/// A single-slot plan: the start directory only, no global slot.
fn plan() -> DiscoveryPlan {
    DiscoveryPlan::new(identity(&["project"]), None, file_name(CONFIG))
}

fn config_path() -> std::path::PathBuf {
    abs(&["project", CONFIG])
}

/// The outcome at the start directory, under a walk bounded to that directory
/// ALONE.
///
/// Depth one on purpose: these witnesses count probes and reads, and a walk that
/// also visited the root would probe a second slot and make the count a
/// statement about the tree's shape rather than about the bound under test.
fn outcome_at_start(reader: &DeterministicSourceReader, bytes: usize) -> SourceOutcome {
    let bounds = custom_source_bounds(bytes, 1, 8, 4096);
    let census = match discover(&plan(), reader, bounds) {
        Ok(census) => census,
        Err(refusal) => panic!("discovery refused: {refusal}"),
    };
    let found = census
        .entries()
        .iter()
        .find(|entry| entry.slot() == SourceSlot::Ancestor { distance: 0 })
        .map(|entry| entry.outcome().clone());
    match found {
        Some(outcome) => outcome,
        None => panic!("the start directory slot is missing from the census"),
    }
}

/// Over-bound at probe means refused with NO read attempted.
///
/// The read count is the whole witness. An implementation that read the
/// document and then measured it would produce the same outcome value and
/// would have allocated for the bytes the bound exists to refuse.
#[test]
fn an_oversized_document_is_refused_at_probe_without_a_read() {
    let reader = DeterministicSourceReader::new().with_file(&config_path(), vec![b'x'; 100]);

    assert_eq!(
        outcome_at_start(&reader, 10),
        SourceOutcome::TooLargeAtProbe {
            actual: 100,
            bound: 10
        }
    );
    assert_eq!(
        reader.probes(),
        1,
        "the document must have been probed exactly once"
    );
    assert_eq!(
        reader.reads(),
        0,
        "an over-bound document must be refused BEFORE the read that would allocate for it"
    );
}

/// The refuse-before-read ORDERING, isolated from the outcome it produces.
///
/// The witness above asserts the outcome first, so an implementation that
/// deleted the probe bound entirely reddens it on the OUTCOME and never reaches
/// its read-count assertion. That leaves the ordering half — refused BEFORE the
/// read, not merely refused — carried by an assertion no mutant can reach
/// first.
///
/// This witness exists to be reddened by the one edit the other cannot catch:
/// hoisting the read above the unchanged bound check. Such an implementation
/// returns the SAME `TooLargeAtProbe` value and probes exactly once, so every
/// assertion here except the read count still passes, and the read count is
/// what fires. The `matches!` guard is not the law — it is there so this
/// witness fails loudly if it ever stops driving the over-bound path at all,
/// rather than passing vacuously on a document that was never oversized.
#[test]
fn an_oversized_document_is_refused_before_any_read_is_attempted() {
    let reader = DeterministicSourceReader::new().with_file(&config_path(), vec![b'x'; 100]);

    let outcome = outcome_at_start(&reader, 10);

    assert!(
        matches!(outcome, SourceOutcome::TooLargeAtProbe { .. }),
        "this witness must drive the over-bound refusal path, found {outcome:?}"
    );
    assert_eq!(
        reader.probes(),
        1,
        "the document must have been probed exactly once"
    );
    assert_eq!(
        reader.reads(),
        0,
        "the refusal must happen BEFORE the read: a read that runs and is then          discarded has already allocated for the bytes the bound exists to refuse"
    );
}

/// The paired positive for the probe bound: a document exactly at the bound is
/// captured.
///
/// Without it, a mutant that refused every document — the strengthening that
/// hides the weakening — would keep the refusal witness above green.
#[test]
fn a_document_exactly_at_the_bound_is_captured() {
    let reader = DeterministicSourceReader::new().with_file(&config_path(), vec![b'x'; 10]);

    let outcome = outcome_at_start(&reader, 10);
    assert_eq!(
        outcome.bytes(),
        Some(&vec![b'x'; 10][..]),
        "a document of exactly the bound's length is within the bound"
    );
    assert_eq!(reader.reads(), 1);
}

/// A document that fits at probe and grows before the read is still refused.
///
/// The probe is a hint about a file another process may be appending to. If the
/// read were uncapped, this document would be captured at ten times the bound
/// and every downstream bound would be a bound over an unbounded allocation.
///
/// The outcome is its OWN variant rather than the probe-time one, so a witness
/// can tell which of the two checks fired. Sharing a variant would let the read
/// cap be deleted with this test still green on the probe-time path.
#[test]
fn a_document_that_grows_between_probe_and_read_is_still_refused() {
    let reader =
        DeterministicSourceReader::new().with_growing_file(&config_path(), 4, vec![b'x'; 100]);

    assert_eq!(
        outcome_at_start(&reader, 10),
        SourceOutcome::GrewPastBound { bound: 10 },
        "a document that outgrew its probe must be refused by the read cap"
    );
    assert_eq!(
        reader.reads(),
        1,
        "the read must have been attempted: this is the cap firing, not the probe"
    );
}

/// Absent, vanished and unreadable are three outcomes, not one.
///
/// A file that was never written, a file that was removed between the probe and
/// the read, and a file that exists and cannot be opened are three different
/// facts about the world. Collapsing them is how a permission error becomes a
/// silent fall back to defaults that nobody configured.
#[test]
fn absent_vanished_and_unreadable_are_three_distinct_outcomes() {
    let vanishing = abs(&["home", "example", CONFIG]);
    let unreadable = abs(&["home", CONFIG]);

    let reader = DeterministicSourceReader::new()
        .with_vanishing_file(&vanishing, 2)
        .with_unreadable_file(&unreadable, io::ErrorKind::PermissionDenied);

    let plan = DiscoveryPlan::new(
        identity(&["home", "example", "project"]),
        None,
        file_name(CONFIG),
    );
    let census = match discover(&plan, &reader, source_bounds()) {
        Ok(census) => census,
        Err(refusal) => panic!("discovery refused: {refusal}"),
    };

    let outcome_of = |distance: usize| {
        census
            .entries()
            .iter()
            .find(|entry| entry.slot() == SourceSlot::Ancestor { distance })
            .map(|entry| entry.outcome().clone())
    };

    assert_eq!(
        outcome_of(0),
        Some(SourceOutcome::Absent),
        "the start directory holds no document and was probed"
    );
    assert_eq!(
        outcome_of(1),
        Some(SourceOutcome::Vanished),
        "a document present at probe and gone at read has vanished, not been absent"
    );
    assert_eq!(
        outcome_of(2),
        Some(SourceOutcome::Unreadable {
            kind: io::ErrorKind::PermissionDenied
        }),
        "a present, unopenable document is unreadable, not absent"
    );

    let kinds: Vec<&str> = census
        .entries()
        .iter()
        .map(|entry| entry.outcome().kind())
        .collect();
    assert!(
        kinds.contains(&"absent") && kinds.contains(&"vanished") && kinds.contains(&"unreadable"),
        "the three states must remain nameable apart: {kinds:?}"
    );
}

/// A directory sitting where a document was expected is not a document.
///
/// It is also not absent: the path is occupied, and an author who made that
/// mistake is told so rather than told nothing.
#[test]
fn a_directory_at_a_config_path_is_not_a_document() {
    let reader = DeterministicSourceReader::new().with_directory(&config_path());

    assert_eq!(outcome_at_start(&reader, 1024), SourceOutcome::NotAFile);
    assert_eq!(
        reader.reads(),
        0,
        "a directory is refused from its probe; there is nothing to read"
    );
}

/// Every slot is reported, including the ones that held nothing.
///
/// A census is not a list of documents. A consumer that must explain why a
/// setting did not take effect needs to see that a place WAS looked at, which a
/// list of what was found cannot express.
#[test]
fn the_census_reports_every_slot_and_not_only_the_ones_that_held_a_document() {
    let reader = DeterministicSourceReader::new().with_file(&config_path(), b"{}".to_vec());
    let bounds = custom_source_bounds(1024, 4, 8, 4096);
    let census = match discover(&plan(), &reader, bounds) {
        Ok(census) => census,
        Err(refusal) => panic!("discovery refused: {refusal}"),
    };

    assert_eq!(
        census.captured().count(),
        1,
        "exactly one document was declared"
    );
    assert_eq!(
        census.entries().len(),
        3,
        "the global slot and both walk slots are reported"
    );
    assert!(
        census
            .entries()
            .iter()
            .any(|entry| matches!(entry.outcome(), SourceOutcome::Absent)),
        "a probed slot that held nothing must be reported as absent"
    );
}

/// The PRODUCTION reader's cap, on a real file.
///
/// Every witness above runs against the declared reader, which proves what
/// `capture` does with a refusal and nothing about whether `StdSourceReader`
/// produces one. This is the other half: a real file on disk, read through the
/// real adapter, larger than the bound.
///
/// The directory is `CARGO_TARGET_TMPDIR` — cargo's own per-package scratch
/// space. Not `std::env::temp_dir`, which is the ambient input this lane
/// refuses, and not a fresh dependency for one witness.
#[test]
fn the_production_reader_caps_its_read_rather_than_trusting_the_probe() {
    use grapt_config::{MaxDocumentBytes, SourceIoError, SourceReader, StdSourceReader};
    use support::source_support::path_bound;

    let directory = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("u1b1-read-cap");
    if let Err(error) = std::fs::create_dir_all(&directory) {
        panic!("cannot prepare the scratch directory: {error}");
    }
    let file = directory.join("oversized.jsonc");
    if let Err(error) = std::fs::write(&file, vec![b'x'; 100]) {
        panic!("cannot write the scratch document: {error}");
    }

    let Ok(identity) = grapt_config::ConfigSourcePath::new(&file, path_bound()) else {
        panic!("the scratch path was refused");
    };
    let Ok(bound) = MaxDocumentBytes::try_new(10) else {
        panic!("a ten byte bound must be admitted");
    };
    let reader = StdSourceReader;

    let probe = match reader.probe(&identity) {
        Ok(probe) => probe,
        Err(error) => panic!("the scratch document could not be probed: {error}"),
    };
    assert_eq!(probe.byte_len(), 100);
    assert!(probe.is_file());

    assert_eq!(
        reader.read_bounded(&identity, bound),
        Err(SourceIoError::ExceededBound { bound: 10 }),
        "the production read must refuse an over-bound document rather than truncate it"
    );

    // The paired positive: a document within the bound reads back whole. A cap
    // that refused everything would keep the refusal above green.
    let small = directory.join("small.jsonc");
    if let Err(error) = std::fs::write(&small, b"{}") {
        panic!("cannot write the scratch document: {error}");
    }
    let Ok(small_identity) = grapt_config::ConfigSourcePath::new(&small, path_bound()) else {
        panic!("the scratch path was refused");
    };
    assert_eq!(
        reader.read_bounded(&small_identity, bound),
        Ok(b"{}".to_vec()),
        "a document within the bound must read back whole"
    );
}
