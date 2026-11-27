//! Ordered contribution: which documents speak to a path, in what order, and
//! where each one came from.
//!
//! The load-bearing witness in this file is
//! [`two_contributors_at_one_path_are_reported_separately_and_never_merged`].
//! Merging and ordered contribution agree on every fixture where only one
//! document authors a path, so only a witness whose two documents author
//! DIFFERENT keys under one path can tell them apart. A merging implementation
//! answers such a lookup with one combined object; this crate answers with two
//! contributions and lets the domain fold decide, because deciding it here
//! would be deciding it without knowing what the setting means.

use crate::support;

use grapt_config::{
    admit, discover, CompleteLedger, ConfigValue, DeterministicSourceReader, DiscoveryPlan,
    Resolution, SourceSlot,
};
use support::custom_bounds;
use support::source_support::{abs, custom_source_bounds, file_name, identity};

const CONFIG: &str = "config.jsonc";

fn plan() -> DiscoveryPlan {
    DiscoveryPlan::new(
        identity(&["project", "child"]),
        Some(identity(&["global"])),
        file_name(CONFIG),
    )
}

fn global_path() -> std::path::PathBuf {
    abs(&["global", CONFIG])
}

fn parent_path() -> std::path::PathBuf {
    abs(&["project", CONFIG])
}

fn child_path() -> std::path::PathBuf {
    abs(&["project", "child", CONFIG])
}

fn complete(reader: &DeterministicSourceReader) -> CompleteLedger {
    let census = match discover(
        &plan(),
        reader,
        custom_source_bounds(1024 * 1024, 2, 64, 4096),
    ) {
        Ok(census) => census,
        Err(refusal) => panic!("discovery refused: {refusal}"),
    };
    let ledger = match admit(&census, custom_bounds(1024 * 1024, 32, 64, 32, 10_000)) {
        Ok(ledger) => ledger,
        Err(refusal) => panic!("the ledger refused: {refusal}"),
    };
    match ledger.complete() {
        Ok(complete) => complete,
        Err(refusal) => panic!("the ledger is incomplete: {refusal}"),
    }
}

/// Every integer contributed at `path`, weakest contributor first.
fn numbers_at(resolution: &Resolution<'_>, path: &[&str]) -> Vec<i64> {
    resolution
        .contributions_at(path)
        .iter()
        .map(|contribution| match contribution.value() {
            ConfigValue::Number(number) => match number.as_i64() {
                Some(value) => value,
                None => panic!("the number at {path:?} is not an exact integer"),
            },
            other => panic!("expected a number at {path:?}, found {other:?}"),
        })
        .collect()
}

/// Contributions arrive weakest-first, and the nearest slot is LAST.
///
/// The order is the whole product: a caller folding front-to-back gets
/// nearest-wins without this crate having decided that for it.
#[test]
fn contributions_arrive_weakest_first_with_the_nearest_slot_last() {
    let reader = DeterministicSourceReader::new()
        .with_file(&global_path(), br#"{"model": 1}"#.to_vec())
        .with_file(&parent_path(), br#"{"model": 2}"#.to_vec())
        .with_file(&child_path(), br#"{"model": 3}"#.to_vec());

    let ledger = complete(&reader);
    let resolution = grapt_config::resolve(&ledger);

    assert_eq!(
        numbers_at(&resolution, &["model"]),
        vec![1, 2, 3],
        "global, then parent, then the start directory"
    );
}

/// Order follows the SLOT, not the value and not the order documents were
/// found.
///
/// The weakest slot carries the largest value here, so an implementation that
/// ordered by the value itself, or that emitted an unordered ledger, produces
/// `[3, 9]` instead of `[9, 3]`.
#[test]
fn order_is_the_slot_order_and_not_the_value_or_the_visit_order() {
    let reader = DeterministicSourceReader::new()
        .with_file(&global_path(), br#"{"model": 9}"#.to_vec())
        .with_file(&child_path(), br#"{"model": 3}"#.to_vec());

    let ledger = complete(&reader);
    assert_eq!(
        numbers_at(&grapt_config::resolve(&ledger), &["model"]),
        vec![9, 3]
    );
}

/// THE LAW OF THIS FILE. Two contributors that both author an object under one
/// key are reported as TWO contributions, and no merged object is ever
/// produced.
///
/// A config-owned recursive merge answers `["provider"]` with one object
/// carrying both `endpoint` and `token`, and answers `["provider", "endpoint"]`
/// with the weaker document's value even though the nearer document never
/// authored that path. Both are out of this crate's contract. Here each
/// contributor answers only for what it actually wrote.
#[test]
fn two_contributors_at_one_path_are_reported_separately_and_never_merged() {
    let reader = DeterministicSourceReader::new()
        .with_file(
            &global_path(),
            br#"{"provider": {"endpoint": 1, "token": 2}}"#.to_vec(),
        )
        .with_file(&child_path(), br#"{"provider": {"token": 7}}"#.to_vec());

    let ledger = complete(&reader);
    let resolution = grapt_config::resolve(&ledger);

    let provider = resolution.contributions_at(&["provider"]);
    assert_eq!(
        provider.len(),
        2,
        "both documents author an object at `provider`, and both are reported"
    );

    // The weaker contributor's object is exactly what its author wrote: two
    // keys. A merge would have folded the nearer document's `token` into it.
    let ConfigValue::Object(weaker) = provider[0].value() else {
        panic!("the weaker contribution at `provider` must be an object");
    };
    let weaker_keys: Vec<&str> = weaker.iter().map(|(key, _)| key.value().as_str()).collect();
    assert_eq!(weaker_keys, vec!["endpoint", "token"]);

    // The nearer contributor's object is ALSO exactly what its author wrote:
    // one key. A merge would have carried `endpoint` into it, which is the
    // precise falsehood — the nearer file never mentioned `endpoint`.
    let ConfigValue::Object(nearer) = provider[1].value() else {
        panic!("the nearer contribution at `provider` must be an object");
    };
    let nearer_keys: Vec<&str> = nearer.iter().map(|(key, _)| key.value().as_str()).collect();
    assert_eq!(
        nearer_keys,
        vec!["token"],
        "the nearer document authored ONE key; nothing may be folded into it"
    );

    // And the sibling key belongs to exactly one contributor, not to a merged
    // result that both appear to have supplied.
    assert_eq!(
        numbers_at(&resolution, &["provider", "endpoint"]),
        vec![1],
        "only the document that authored `endpoint` contributes there"
    );
    assert_eq!(
        numbers_at(&resolution, &["provider", "token"]),
        vec![2, 7],
        "both authored `token`, weakest first"
    );
}

/// A contributor that does not author a path is ABSENT from the result, not
/// reported as null.
///
/// "This file says nothing here" and "this file says null here" are different
/// statements, and a fold that could not tell them apart could not implement
/// a clearing `null` even if its domain called for one.
#[test]
fn a_contributor_that_does_not_author_a_path_is_absent_rather_than_null() {
    let reader = DeterministicSourceReader::new()
        .with_file(&global_path(), br#"{"a": 1}"#.to_vec())
        .with_file(&child_path(), br#"{"b": 2}"#.to_vec());

    let ledger = complete(&reader);
    let resolution = grapt_config::resolve(&ledger);

    assert_eq!(resolution.contributions_at(&["a"]).len(), 1);
    assert_eq!(resolution.contributions_at(&["b"]).len(), 1);
    assert!(
        resolution.contributions_at(&["c"]).is_empty(),
        "a path nobody authored has no contributions at all"
    );
}

/// A lookup never descends through a non-object, and never borrows another
/// contributor's shape to do so.
///
/// The weaker document authors an object at `x`; the nearer authors a scalar.
/// `["x", "deep"]` is authored by exactly one of them.
#[test]
fn a_lookup_does_not_descend_through_a_non_object() {
    let reader = DeterministicSourceReader::new()
        .with_file(&global_path(), br#"{"x": {"deep": 4}}"#.to_vec())
        .with_file(&child_path(), br#"{"x": 5}"#.to_vec());

    let ledger = complete(&reader);
    let resolution = grapt_config::resolve(&ledger);

    assert_eq!(resolution.contributions_at(&["x"]).len(), 2);
    assert_eq!(
        numbers_at(&resolution, &["x", "deep"]),
        vec![4],
        "the contributor whose `x` is a scalar contributes nothing under it"
    );
}

/// Every contribution carries the contributor that supplied it.
///
/// Provenance is per contribution and is the slot-derived identity, so a caller
/// that folded these can say afterwards which file it obeyed.
#[test]
fn each_contribution_carries_the_contributor_that_supplied_it() {
    let reader = DeterministicSourceReader::new()
        .with_file(&global_path(), br#"{"model": 1}"#.to_vec())
        .with_file(&child_path(), br#"{"model": 3}"#.to_vec());

    let ledger = complete(&reader);
    let resolution = grapt_config::resolve(&ledger);
    let contributions = resolution.contributions_at(&["model"]);
    assert_eq!(contributions.len(), 2);

    assert_eq!(contributions[0].from().slot(), SourceSlot::Global);
    assert_eq!(
        contributions[0].from().path().as_path(),
        global_path().as_path(),
        "the weaker contribution names the global file"
    );

    assert!(
        matches!(contributions[1].from().slot(), SourceSlot::Ancestor { .. }),
        "the nearer contribution comes from an ancestor slot"
    );
    assert_eq!(
        contributions[1].from().path().as_path(),
        child_path().as_path(),
        "the nearer contribution names the start directory's file"
    );
    assert!(
        contributions[1].from().rank().value() > contributions[0].from().rank().value(),
        "ranks ascend with precedence"
    );
}

/// Every contributor's whole document is reachable, weakest first.
#[test]
fn the_contributors_are_the_whole_documents_in_ascending_precedence() {
    let reader = DeterministicSourceReader::new()
        .with_file(&global_path(), br#"{"a": 1}"#.to_vec())
        .with_file(&parent_path(), br#"{"a": 2}"#.to_vec())
        .with_file(&child_path(), br#"{"a": 3}"#.to_vec());

    let ledger = complete(&reader);
    let resolution = grapt_config::resolve(&ledger);

    assert_eq!(resolution.len(), 3);
    let ranks: Vec<usize> = resolution
        .contributors()
        .map(|contribution| contribution.from().rank().value())
        .collect();
    let mut ascending = ranks.clone();
    ascending.sort_unstable();
    assert_eq!(ranks, ascending, "contributors ascend in precedence");
}

/// The empty case: no documents is a COMPLETE generation with no contributors.
///
/// Every slot was looked at and none held a file. That is an answer, not a
/// failure, and it must not be confused with a slot that could not be read.
#[test]
fn a_tree_with_no_documents_is_complete_and_has_no_contributors() {
    let reader = DeterministicSourceReader::new();

    let ledger = complete(&reader);
    let resolution = grapt_config::resolve(&ledger);

    assert!(resolution.is_empty());
    assert_eq!(resolution.len(), 0);
    assert!(resolution.contributions_at(&["anything"]).is_empty());
}

/// Guard: the fixtures above are meaningful only if the number helper reads the
/// authored integer rather than defaulting to one.
///
/// Every witness above asserts WHICH numbers were contributed, so a helper that
/// silently produced the same value for any document would make all of them
/// pass without observing anything.
#[test]
fn the_number_helper_reads_the_authored_integer() {
    let reader =
        DeterministicSourceReader::new().with_file(&child_path(), br#"{"n": 42}"#.to_vec());
    let ledger = complete(&reader);
    assert_eq!(
        numbers_at(&grapt_config::resolve(&ledger), &["n"]),
        vec![42]
    );
}
