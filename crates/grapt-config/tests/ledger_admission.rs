//! A captured document either becomes a contributor or becomes a typed reason
//! why it did not — and never becomes nothing at all.
//!
//! Every witness here runs against a DECLARED tree, so admission is a function
//! of the fixture rather than of the machine the gate runs on.
//!
//! The load-bearing witness in this file is
//! [`an_undecodable_document_is_kept_as_an_invalid_contributor`]. An
//! implementation that dropped undecodable captures would pass every witness
//! that only ever looks at a tree of well-formed documents, and would make a
//! syntax error in a config file indistinguishable from that file being absent.

use crate::support;

use grapt_config::{
    admit, discover, ConfigLedger, ConfigValue, DeterministicSourceReader, DiscoveryPlan,
    IncompleteLedger, InvalidContributor, LedgerRefusal, SourceSlot,
};
use support::custom_bounds;
use support::source_support::{abs, custom_source_bounds, file_name, identity};

const CONFIG: &str = "config.jsonc";

fn start_parts() -> [&'static str; 2] {
    ["project", "child"]
}

fn plan() -> DiscoveryPlan {
    DiscoveryPlan::new(identity(&start_parts()), None, file_name(CONFIG))
}

fn child_path() -> std::path::PathBuf {
    abs(&["project", "child", CONFIG])
}

fn parent_path() -> std::path::PathBuf {
    abs(&["project", CONFIG])
}

/// A ledger over a two-slot walk with generous decode bounds.
fn ledger(reader: &DeterministicSourceReader) -> ConfigLedger {
    ledger_with(reader, 64)
}

/// A ledger whose DOCUMENT-COUNT bound is the parameter under test.
///
/// The source bounds keep their own generous document allowance so the walk
/// itself is never the thing that refuses: this file's bound witness is about
/// the ledger's cap, and a discovery that refused first would prove nothing
/// about it.
fn ledger_with(reader: &DeterministicSourceReader, documents: usize) -> ConfigLedger {
    match try_ledger(reader, documents) {
        Ok(ledger) => ledger,
        Err(refusal) => panic!("the ledger refused: {refusal}"),
    }
}

fn try_ledger(
    reader: &DeterministicSourceReader,
    documents: usize,
) -> Result<ConfigLedger, LedgerRefusal> {
    let census = match discover(
        &plan(),
        reader,
        custom_source_bounds(1024 * 1024, 2, 64, 4096),
    ) {
        Ok(census) => census,
        Err(refusal) => panic!("discovery refused: {refusal}"),
    };
    admit(
        &census,
        custom_bounds(1024 * 1024, 32, documents, 32, 10_000),
    )
}

/// A document that decodes and is an object becomes a contributor.
///
/// The paired positive for every refusal below: without it, an implementation
/// that admitted NOTHING would keep all of them green.
#[test]
fn a_well_formed_object_document_becomes_an_admitted_contributor() {
    let reader = DeterministicSourceReader::new().with_file(&child_path(), br#"{"a": 1}"#.to_vec());

    let ledger = ledger(&reader);

    assert_eq!(
        ledger.entries().len(),
        1,
        "one captured slot, one ledger row"
    );
    assert_eq!(ledger.admitted().count(), 1);
    assert_eq!(ledger.invalid().count(), 0);

    let Some(entry) = ledger.admitted().next() else {
        panic!("the admitted contributor is missing");
    };
    assert_eq!(entry.slot(), SourceSlot::Ancestor { distance: 0 });
    assert_eq!(entry.bytes(), Some(&br#"{"a": 1}"#[..]));
    let Some(document) = entry.document() else {
        panic!("an admitted contributor must carry its document");
    };
    assert!(matches!(document.value(), ConfigValue::Object(_)));
}

/// A capture that does not decode is KEPT, with its coordinates and its reason.
///
/// This is the law the whole module exists for. The assertion is deliberately
/// about the row still being there — its slot, its rank, its path — and not
/// merely about a count, because an implementation that recorded a bare "one
/// document failed" tally would satisfy a count and still lose which file the
/// author has to go fix.
#[test]
fn an_undecodable_document_is_kept_as_an_invalid_contributor() {
    let reader =
        DeterministicSourceReader::new().with_file(&child_path(), b"{ not json at all".to_vec());

    let ledger = ledger(&reader);

    assert_eq!(
        ledger.entries().len(),
        1,
        "an undecodable capture must still occupy its ledger row"
    );
    assert_eq!(
        ledger.admitted().count(),
        0,
        "an undecodable document must not be admitted"
    );

    let Some(entry) = ledger.invalid().next() else {
        panic!("the invalid contributor was dropped instead of kept");
    };
    assert_eq!(entry.slot(), SourceSlot::Ancestor { distance: 0 });
    assert_eq!(entry.path().as_path(), child_path());
    assert!(
        matches!(entry.invalid(), Some(InvalidContributor::Undecodable(_))),
        "the reason must say it did not decode, found {:?}",
        entry.invalid()
    );
    assert_eq!(
        entry.document(),
        None,
        "an invalid contributor has no document"
    );
    assert_eq!(
        entry.bytes(),
        None,
        "an invalid contributor exposes no captured bytes"
    );
}

/// A document that decodes to something other than an object is invalid, and
/// says so in a different way than an undecodable one.
///
/// Distinct from `Undecodable` on purpose: the author of `[1, 2]` wrote valid
/// JSONC, and telling them it failed to decode would send them looking for a
/// syntax error that is not there.
#[test]
fn a_document_whose_top_level_is_not_an_object_is_a_distinct_invalid_reason() {
    let reader = DeterministicSourceReader::new().with_file(&child_path(), b"[1, 2]".to_vec());

    let ledger = ledger(&reader);

    let Some(entry) = ledger.invalid().next() else {
        panic!("a non-object document must be kept as an invalid contributor");
    };
    assert_eq!(
        entry.invalid(),
        Some(&InvalidContributor::NotAnObject { kind: "array" }),
        "the reason must name what was found instead of an object"
    );
}

/// Only a CAPTURED slot becomes a ledger row.
///
/// The census already distinguishes absent, unreadable and the rest. Recording
/// them here as well would build a second population of the same facts, and two
/// populations of the same facts drift apart.
#[test]
fn a_slot_that_captured_no_document_produces_no_ledger_row() {
    let reader = DeterministicSourceReader::new()
        .with_file(&child_path(), br#"{"a": 1}"#.to_vec())
        .with_unreadable_file(&parent_path(), std::io::ErrorKind::PermissionDenied);

    let ledger = ledger(&reader);

    assert_eq!(
        ledger.entries().len(),
        1,
        "the unreadable slot is a census fact, not a ledger row"
    );
    assert_eq!(
        ledger.entries()[0].slot(),
        SourceSlot::Ancestor { distance: 0 },
        "the row that exists is the captured one"
    );
}

/// The document-count bound refuses the whole ledger rather than truncating it.
///
/// A truncating implementation would return a ledger that reads like a complete
/// one, which is how a configuration silently loses its weakest contributors.
#[test]
fn a_census_over_the_document_bound_is_refused_whole() {
    let reader = DeterministicSourceReader::new()
        .with_file(&child_path(), br#"{"a": 1}"#.to_vec())
        .with_file(&parent_path(), br#"{"b": 2}"#.to_vec());

    match try_ledger(&reader, 1) {
        Ok(ledger) => panic!(
            "two captured documents were admitted under a bound of one: {} rows",
            ledger.entries().len()
        ),
        Err(LedgerRefusal::TooManyDocuments { found, bound }) => {
            assert_eq!(found, 2);
            assert_eq!(bound, 1);
        }
    }
}

/// The paired positive for the bound: a census exactly AT the cap is admitted.
///
/// Without it, a mutant that refused every census — the strengthening that
/// hides the weakening — would keep the refusal witness above green.
#[test]
fn a_census_exactly_at_the_document_bound_is_admitted() {
    let reader = DeterministicSourceReader::new()
        .with_file(&child_path(), br#"{"a": 1}"#.to_vec())
        .with_file(&parent_path(), br#"{"b": 2}"#.to_vec());

    let ledger = ledger_with(&reader, 2);

    assert_eq!(ledger.entries().len(), 2);
    assert_eq!(ledger.admitted().count(), 2);
}

// ---------------------------------------------------------------------------
// The completeness boundary over OBSTRUCTED slots.
//
// A slot that could not be READ is a different failure from a document that
// could not be DECODED, and both must refuse the same way: an incomplete
// configuration reaches nothing. These witness `ConfigLedger::complete`
// directly rather than through any consumer, because the law is the ledger's
// and a witness that reached it through a caller would also pass if the caller
// happened to refuse for its own reasons.
//
// The distinction that makes these load-bearing: an obstructed slot admits NO
// ledger row, exactly like an absent one. Every witness that only counts
// contributors sees the two as identical, so only a witness that asserts the
// typed refusal can tell "this slot holds nothing" from "nobody can say what
// this slot holds".
// ---------------------------------------------------------------------------

/// An UNREADABLE slot refuses the whole ledger.
///
/// The weaker document here is perfectly admissible on its own, which is what
/// makes the fixture sharp: an implementation that completed over the readable
/// subset would produce a ledger that looks entirely healthy, and the file the
/// author could not read would simply not be in it.
#[test]
fn an_unreadable_slot_refuses_completion() {
    let reader = DeterministicSourceReader::new()
        .with_file(&parent_path(), br#"{"model": 1}"#.to_vec())
        .with_unreadable_file(&child_path(), std::io::ErrorKind::PermissionDenied);

    let ledger = ledger(&reader);
    assert_eq!(
        ledger.invalid().count(),
        0,
        "precondition: nothing failed to DECODE; a slot could not be read at all"
    );
    assert_eq!(
        ledger.admitted().count(),
        1,
        "precondition: the readable document did become a contributor"
    );

    let refusal = match ledger.complete() {
        Ok(_) => panic!("an unreadable slot must refuse completion"),
        Err(refusal) => refusal,
    };
    let IncompleteLedger::Obstructed {
        obstructed, kind, ..
    } = &refusal
    else {
        panic!("the refusal must name the obstruction, found {refusal:?}");
    };
    assert_eq!(*obstructed, 1);
    assert_eq!(*kind, "unreadable");
}

/// A VANISHED document refuses the whole ledger.
///
/// Vanished is a RACE, not a configuration state: the document was there at
/// probe time and gone at read time. Completing over it would report the
/// author's file as one they never wrote.
#[test]
fn a_vanished_document_refuses_completion() {
    let reader = DeterministicSourceReader::new()
        .with_file(&parent_path(), br#"{"model": 1}"#.to_vec())
        .with_vanishing_file(&child_path(), 12);

    let ledger = ledger(&reader);
    assert_eq!(
        ledger.invalid().count(),
        0,
        "precondition: the vanished slot produced no ledger row to be invalid"
    );

    let refusal = match ledger.complete() {
        Ok(_) => panic!("a vanished document must refuse completion"),
        Err(refusal) => refusal,
    };
    let IncompleteLedger::Obstructed { kind, .. } = &refusal else {
        panic!("the refusal must name the obstruction, found {refusal:?}");
    };
    assert_eq!(*kind, "vanished");
}

/// The paired POSITIVE: absent, not-configured and duplicate are ANSWERS about
/// the configuration and complete normally.
///
/// Without this, both witnesses above are satisfied by an implementation that
/// refuses every ledger — a strengthening that hides as a pass. Four of the
/// nine source outcomes produce no contributor, and only some of them are
/// failures.
#[test]
fn absent_and_unconfigured_slots_complete_normally() {
    let reader = DeterministicSourceReader::new()
        .with_directory(&abs(&["project", "child"]))
        .with_file(&parent_path(), br#"{"model": 1}"#.to_vec());

    let ledger = ledger(&reader);
    assert_eq!(ledger.admitted().count(), 1);

    let complete = match ledger.complete() {
        Ok(complete) => complete,
        Err(refusal) => panic!("an absent slot is an answer, not a refusal: {refusal}"),
    };
    assert_eq!(complete.contributors().count(), 1);
}

/// A tree with NO documents at all completes, with no contributors.
#[test]
fn a_tree_with_no_documents_completes_with_no_contributors() {
    let reader = DeterministicSourceReader::new().with_directory(&abs(&["project", "child"]));

    let complete = match ledger(&reader).complete() {
        Ok(complete) => complete,
        Err(refusal) => panic!("an empty tree is a complete answer: {refusal}"),
    };
    assert_eq!(complete.contributors().count(), 0);
}
