//! Which slots exist, in what order, and what number each one carries.
//!
//! Every witness here runs against a DECLARED tree — `DeterministicSourceReader`
//! — so a discovery result is a function of the fixture and never of the machine
//! the gate happens to run on.
//!
//! The load-bearing witness in this file is
//! [`a_slot_rank_does_not_move_when_a_neighbouring_document_appears`]. Rank
//! derived from the slot and rank derived from the found-document count agree on
//! every fully-populated tree, so only a witness that discovers the SAME tree
//! twice, with one file added, can tell them apart.

use crate::support;

use grapt_config::{
    discover, ConfigSourcePath, DeterministicSourceReader, DiscoveryPlan, PathRefusal,
    SourceCensus, SourceOutcome, SourceSlot,
};
use support::source_support::{
    abs, custom_source_bounds, file_name, identity, path_bound, source_bounds, try_source_bounds,
};

const CONFIG: &str = "config.jsonc";

/// `<root>/home/example/project/child`, four names below the platform root.
fn start_parts() -> [&'static str; 4] {
    ["home", "example", "project", "child"]
}

fn plan(global: Option<ConfigSourcePath>) -> DiscoveryPlan {
    DiscoveryPlan::new(identity(&start_parts()), global, file_name(CONFIG))
}

fn census(reader: &DeterministicSourceReader, global: Option<ConfigSourcePath>) -> SourceCensus {
    match discover(&plan(global), reader, source_bounds()) {
        Ok(census) => census,
        Err(refusal) => panic!("discovery refused: {refusal}"),
    }
}

fn slots(census: &SourceCensus) -> Vec<SourceSlot> {
    census.entries().iter().map(|entry| entry.slot()).collect()
}

/// The order IS the precedence, and it ascends.
///
/// The global slot first, then every ancestor from the most distant, then the
/// start directory last. A consumer folds this front-to-back and gets
/// nearest-wins without having to know that is what it is doing.
///
/// The walk reaches the platform root and stops there rather than at the depth
/// bound, which is why the most distant ancestor is distance 4 and not 31.
#[test]
fn the_census_runs_farthest_to_nearest_with_the_global_slot_first() {
    let reader = DeterministicSourceReader::new();
    let census = census(&reader, Some(identity(&["home", "example", ".config"])));

    assert_eq!(
        slots(&census),
        vec![
            SourceSlot::Global,
            SourceSlot::Ancestor { distance: 4 },
            SourceSlot::Ancestor { distance: 3 },
            SourceSlot::Ancestor { distance: 2 },
            SourceSlot::Ancestor { distance: 1 },
            SourceSlot::Ancestor { distance: 0 },
        ],
        "the census must run weakest-first: global, farthest ancestor, then inward"
    );
}

/// The walk visits the start directory and stops after `ancestor_depth`
/// directories, counting it.
///
/// A tree deeper than the bound does not produce more slots; it produces the
/// bound's worth. The distant ancestors are ABSENT FROM THE CENSUS rather than
/// present-and-empty, because a slot the bound never admitted was never looked
/// at and reporting it as absent would claim a probe that never happened.
#[test]
fn the_ancestor_walk_stops_at_the_depth_bound() {
    let reader = DeterministicSourceReader::new();
    let bounds = custom_source_bounds(1024, 3, 8, 4096);
    let census = match discover(&plan(None), &reader, bounds) {
        Ok(census) => census,
        Err(refusal) => panic!("discovery refused: {refusal}"),
    };

    assert_eq!(
        slots(&census),
        vec![
            SourceSlot::Global,
            SourceSlot::Ancestor { distance: 2 },
            SourceSlot::Ancestor { distance: 1 },
            SourceSlot::Ancestor { distance: 0 },
        ],
        "a depth bound of 3 admits exactly three walk slots"
    );
    assert_eq!(
        reader.probes(),
        3,
        "only the admitted slots may be probed; the global slot is unconfigured"
    );
}

/// Rank comes from the SLOT. Creating an unrelated file must not renumber it.
///
/// The same tree is discovered twice, differing only by one ancestor document
/// that did not exist in the first run. The start directory's rank is identical
/// across both. An implementation that numbered documents in the order it found
/// them would report 0 and then 1 for the same slot, and every precedence
/// decision a consumer had already made would silently change meaning.
#[test]
fn a_slot_rank_does_not_move_when_a_neighbouring_document_appears() {
    let start_config = abs(&["home", "example", "project", "child", CONFIG]);
    let ancestor_config = abs(&["home", "example", "project", CONFIG]);

    let sparse = DeterministicSourceReader::new().with_file(&start_config, b"{}".to_vec());
    let sparse_census = census(&sparse, None);

    let populated = DeterministicSourceReader::new()
        .with_file(&start_config, b"{}".to_vec())
        .with_file(&ancestor_config, b"{}".to_vec());
    let populated_census = census(&populated, None);

    let rank_of_start = |census: &SourceCensus| {
        census
            .entries()
            .iter()
            .find(|entry| entry.slot() == SourceSlot::Ancestor { distance: 0 })
            .map(|entry| entry.rank().value())
    };

    assert_eq!(
        rank_of_start(&sparse_census),
        rank_of_start(&populated_census),
        "the start directory's rank must not depend on whether a neighbour has a document"
    );
    assert_eq!(
        sparse_census.captured().count(),
        1,
        "the sparse tree holds one document"
    );
    assert_eq!(
        populated_census.captured().count(),
        2,
        "the populated tree holds two"
    );
}

/// Distinct slots take distinct ranks, and the ranks ascend with the census.
///
/// Injectivity is what keeps `grapt_policy::SnapshotBuilder` free of a
/// `PrecedenceConflict`; ascent is what makes the census order and the rank
/// order the same statement rather than two that must be kept in step.
#[test]
fn every_slot_takes_a_distinct_rank_and_ranks_ascend_with_the_census() {
    let reader = DeterministicSourceReader::new();
    let census = census(&reader, Some(identity(&["home", "example", ".config"])));

    let ranks: Vec<usize> = census
        .entries()
        .iter()
        .map(|entry| entry.rank().value())
        .collect();

    assert!(
        ranks.windows(2).all(|pair| pair[0] < pair[1]),
        "ranks must strictly ascend with the census: {ranks:?}"
    );
    let mut unique = ranks.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        ranks.len(),
        "two slots share a rank: {ranks:?}"
    );
}

/// One directory written two ways is one slot's worth of document.
///
/// The global directory here is spelled with two `..` steps and canonicalises
/// onto an ancestor. The document is captured ONCE, at the stronger slot, and
/// the weaker slot reports the rank that took it. Capturing it twice would let
/// one file contribute at two precedences, which is a duplicate contributor
/// wearing two ranks rather than one document.
#[test]
fn two_spellings_of_one_directory_produce_one_captured_document() {
    let shared = abs(&["home", "example", CONFIG]);
    let reader = DeterministicSourceReader::new().with_file(&shared, b"{}".to_vec());

    let spelled_with_parent_steps = match ConfigSourcePath::new(
        &abs(&["home", "example", "project", "child", "..", ".."]),
        path_bound(),
    ) {
        Ok(identity) => identity,
        Err(refusal) => panic!("the parent-step spelling was refused: {refusal}"),
    };
    let census = census(&reader, Some(spelled_with_parent_steps));

    assert_eq!(
        census.captured().count(),
        1,
        "one file must contribute once however it is spelled"
    );

    let ancestor_rank = census
        .entries()
        .iter()
        .find(|entry| entry.slot() == SourceSlot::Ancestor { distance: 2 })
        .map(|entry| entry.rank());
    let Some(ancestor_rank) = ancestor_rank else {
        panic!("the ancestor slot holding the document is missing from the census");
    };

    let global = census.entries().first();
    let Some(global) = global else {
        panic!("the census is empty");
    };
    assert_eq!(global.slot(), SourceSlot::Global);
    assert_eq!(
        global.outcome(),
        &SourceOutcome::Duplicate { of: ancestor_rank },
        "the weaker spelling must defer to the rank that captured the document"
    );
}

/// A bound set whose walk admits more slots than documents is refused at
/// construction.
///
/// This is why `discover` carries no document counter: the bound cannot be
/// exceeded because the bound set that would allow it cannot be built. A runtime
/// counter would be a second place to enforce the same law and a second place to
/// forget it.
#[test]
fn a_bound_set_whose_walk_outruns_its_document_bound_is_refused() {
    let refused = try_source_bounds(1024, 64, 8, 4096);
    let Err(refusal) = refused else {
        panic!("a 65-slot walk under an 8 document bound must be refused");
    };
    assert_eq!(
        refusal.to_string(),
        "a walk of 65 source slots can exceed the 8 document bound"
    );

    // The paired positive: the same walk under a document bound that fits is
    // admitted. Without it, a mutant that refused EVERY bound set would keep
    // the refusal witness green.
    let admitted = try_source_bounds(1024, 64, 65, 4096);
    let Ok(bounds) = admitted else {
        panic!("a 65-slot walk under a 65 document bound must be admitted");
    };
    assert_eq!(bounds.slots(), 65);
}

/// A rank is settled by the slot alone, so the same slot ranks the same under
/// two different bound sets.
///
/// A rank derived from the caller's ancestor depth would make the same file in
/// the same tree compare differently depending on how much of the tree the
/// caller was willing to walk. Two discoveries of one tree, under bound sets
/// that differ only in depth, must agree about every slot they share.
#[test]
fn a_slot_ranks_the_same_under_two_different_bound_sets() {
    let start_config = abs(&["home", "example", "project", "child", CONFIG]);
    let reader = DeterministicSourceReader::new().with_file(&start_config, b"{}".to_vec());

    let shallow = match discover(
        &plan(None),
        &reader,
        custom_source_bounds(1024, 8, 16, 4096),
    ) {
        Ok(census) => census,
        Err(refusal) => panic!("discovery refused: {refusal}"),
    };
    let deep = match discover(
        &plan(None),
        &reader,
        custom_source_bounds(1024, 64, 65, 4096),
    ) {
        Ok(census) => census,
        Err(refusal) => panic!("discovery refused: {refusal}"),
    };

    let ranks = |census: &SourceCensus| -> Vec<(SourceSlot, usize)> {
        census
            .entries()
            .iter()
            .map(|entry| (entry.slot(), entry.rank().value()))
            .collect()
    };
    assert_eq!(
        ranks(&shallow),
        ranks(&deep),
        "the walk reaches the root under both bound sets, so every slot and rank must agree"
    );
}

/// A relative start directory is refused rather than resolved.
///
/// Resolving it would reach for the process's working directory, and the whole
/// slot list — every rank, every path — would become a function of where the
/// binary was launched. That is the ambient input this lane exists to refuse.
#[test]
fn a_relative_start_directory_is_refused_rather_than_resolved() {
    let relative = std::path::Path::new("project").join("child");
    assert_eq!(
        ConfigSourcePath::new(&relative, path_bound()),
        Err(PathRefusal::NotAbsolute)
    );

    // The paired positive: an absolute path with the same tail is admitted, so
    // a mutant that refused every path cannot pass as this law.
    let absolute = identity(&["project", "child"]);
    assert!(absolute.as_path().is_absolute());
}

/// A path may not climb above its own root.
///
/// `..` is folded lexically, and folding past the root would produce a path that
/// names nothing while still looking like a directory to every slot below it.
#[test]
fn a_path_may_not_ascend_above_its_root() {
    let above = abs(&["..", "elsewhere"]);
    assert_eq!(
        ConfigSourcePath::new(&above, path_bound()),
        Err(PathRefusal::AboveRoot)
    );
}

/// An unconfigured global slot is not an absent one.
///
/// Nobody looked, as opposed to somebody looked and found nothing. Collapsing
/// the two would tell a consumer that a user has no global configuration when
/// the truth is that this caller never asked for one.
#[test]
fn an_unconfigured_global_slot_is_not_an_absent_one() {
    let reader = DeterministicSourceReader::new();
    let unconfigured = census(&reader, None);

    let Some(global) = unconfigured.entries().first() else {
        panic!("the census is empty");
    };
    assert_eq!(global.slot(), SourceSlot::Global);
    assert_eq!(global.outcome(), &SourceOutcome::NotConfigured);
    assert!(
        global.path().is_none(),
        "an unconfigured slot names no path to have probed"
    );

    let configured = census(&reader, Some(identity(&["home", "example", ".config"])));
    let Some(configured_global) = configured.entries().first() else {
        panic!("the census is empty");
    };
    assert_eq!(
        configured_global.outcome(),
        &SourceOutcome::Absent,
        "a configured slot with no document is absent, not unconfigured"
    );
}

/// A file name is one path component.
///
/// A name carrying a separator would let the discovery reach outside the
/// directory whose slot it is reported under, and the census would name a rank
/// for a document that does not live there.
#[test]
fn a_config_file_name_may_not_be_a_path() {
    for rejected in ["", ".", "..", "a/b", "a\\b", "/etc/passwd"] {
        assert!(
            grapt_config::ConfigFileName::new(rejected).is_none(),
            "{rejected:?} must be refused as a file name"
        );
    }
    assert!(grapt_config::ConfigFileName::new("config.jsonc").is_some());
}

/// The locator is LEXICAL, and it is not the foundational path identity.
///
/// `grapt_types::PathIdentity` is the one authority/policy/receipt/backend
/// normalization OUTCOME — foundational law row `path`/`hook`/`credential`:
/// "one `PathIdentity` outcome; protected control metadata is removed from
/// writable authority and enforced consistently." This crate's
/// `ConfigSourcePath` is a bounded absolute locator used BEFORE any I/O.
///
/// Two claims, because the boundary fails in two directions. A constructor that
/// resolved through the filesystem would turn every locator into a resolution
/// answer and quietly become the foundational outcome — so a path that is not on
/// disk must still be admitted, with its bytes untouched. And the type must not
/// wear the foundational outcome's NAME on any public surface, because a trace
/// that says `PathIdentity` is a trace a reader will take for an authority
/// answer. Carrying the name was the original defect; it is removed rather than
/// aliased.
#[test]
fn the_config_locator_is_lexical_and_is_not_the_foundational_path_identity() {
    let absent = abs(&["definitely-absent-9f2a4c8e", "nested", "deeper"]);
    assert!(
        !absent.exists(),
        "this witness is vacuous unless the fixture path is genuinely absent"
    );

    let located = match ConfigSourcePath::new(&absent, path_bound()) {
        Ok(located) => located,
        Err(refusal) => panic!("a locator must admit a path that does not exist: {refusal}"),
    };
    assert_eq!(
        located.as_path(),
        absent,
        "a lexical locator returns the bytes it was given, folded and nothing more"
    );

    let rendered = format!("{located:?}");
    assert_eq!(rendered, "ConfigSourcePath(<redacted>)");
    assert!(
        !rendered.contains("PathIdentity"),
        "the locator must not claim the foundational outcome's name: {rendered}"
    );
}

/// No CODE in this crate bears the foundational type's name.
///
/// The rename is only durable if reintroducing the name is caught. The
/// closure asks for exactly this: that no public config surface exports the
/// foundational name. A prose promise cannot hold it, and neither can a test
/// that only looks at the types this file happens to import — the cheapest
/// resurrection is a one-line `pub type PathIdentity = ConfigSourcePath;`
/// added for "compatibility", which every existing witness would pass.
///
/// So this reads the crate's own source. Doc comments MAY name
/// `grapt_types::PathIdentity`, and must be able to: the boundary is explained
/// by naming the thing it is a boundary against, and this module's docs cite it
/// deliberately. Code may not. Line comments are therefore stripped before the
/// check, which is sound here because these sources carry no `//` inside a
/// string literal and no block comments; if either is ever introduced, this
/// witness gets stricter rather than weaker — it would see the code half of the
/// line and still refuse a real occurrence.
#[test]
fn no_code_in_this_crate_bears_the_foundational_path_identity_name() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let entries = match std::fs::read_dir(&src) {
        Ok(entries) => entries,
        Err(error) => panic!("cannot read {}: {error}", src.display()),
    };

    let mut scanned = 0usize;
    for entry in entries {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(error) => panic!("cannot read a source entry: {error}"),
        };
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => panic!("cannot read {}: {error}", path.display()),
        };
        scanned += 1;
        for (index, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            assert!(
                !code.contains("PathIdentity"),
                "{}:{} declares or uses the foundational type's name in CODE: {line}",
                path.display(),
                index + 1
            );
        }
    }

    assert!(
        scanned >= 5,
        "the scan must actually have read this crate's sources, saw {scanned} files"
    );
}
