//! What reaches the effective-authority graph, and what never does.
//!
//! The load-bearing witness in this file is
//! [`an_incomplete_ledger_mints_nothing_at_all`]. Every other witness runs over
//! a tree of well-formed documents, where minting from a complete ledger and
//! minting from the census produce the same layers — only a tree containing a
//! document that did NOT decode can tell them apart, and the difference is
//! whether the authority graph is handed a PARTIAL configuration in which the
//! file the author edited last is the only one missing.
//!
//! The boundary is structural, so the witness asserts the refusal rather than
//! an empty layer list: `mint_layers` names `CompleteLedger`, and the probe in
//! `tests/probes` proves that a `ConfigLedger` cannot be passed to it at all.

use crate::support;

use grapt_policy::{
    AuthorityRealm, ConfigLayer, ConfigSource, ContributionAuthority, KeyVersion, Secret,
};

use grapt_config::{
    admit, discover, mint_layers, CompleteLedger, ConfigLedger, DeterministicSourceReader,
    DiscoveryPlan, IncompleteLedger,
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

fn child_path() -> std::path::PathBuf {
    abs(&["project", "child", CONFIG])
}

fn parent_path() -> std::path::PathBuf {
    abs(&["project", CONFIG])
}

fn raw_ledger(reader: &DeterministicSourceReader) -> ConfigLedger {
    let census = match discover(
        &plan(),
        reader,
        custom_source_bounds(1024 * 1024, 2, 64, 4096),
    ) {
        Ok(census) => census,
        Err(refusal) => panic!("discovery refused: {refusal}"),
    };
    match admit(&census, custom_bounds(1024 * 1024, 32, 64, 32, 10_000)) {
        Ok(ledger) => ledger,
        Err(refusal) => panic!("the ledger refused: {refusal}"),
    }
}

fn ledger(reader: &DeterministicSourceReader) -> CompleteLedger {
    match raw_ledger(reader).complete() {
        Ok(complete) => complete,
        Err(refusal) => panic!("the ledger is incomplete: {refusal}"),
    }
}

/// A fixed injected authority. Fixed on purpose: an identity minted under a
/// per-call key would differ on every run and no witness could compare two.
fn authority() -> ContributionAuthority {
    let Some(realm) = AuthorityRealm::new("grapt-config-witness") else {
        panic!("the witness realm was refused");
    };
    let Some(version) = KeyVersion::new("v1") else {
        panic!("the witness key version was refused");
    };
    ContributionAuthority::new(realm, version, Secret::new(b"witness-key".to_vec()))
}

/// Every admitted contributor becomes exactly one layer, weakest first.
///
/// The order is asserted positionally against layers minted directly at the
/// ledger's own ranks, so an implementation that emitted the right layers in
/// the wrong order — nearest first, or grouped by class — fails here. Comparing
/// whole layers rather than counting them is what makes that observable: a
/// count is equal under any permutation.
#[test]
fn each_admitted_contributor_becomes_one_layer_in_ascending_precedence() {
    let reader = DeterministicSourceReader::new()
        .with_file(&global_path(), br#"{"a": 1}"#.to_vec())
        .with_file(&child_path(), br#"{"b": 2}"#.to_vec());

    let ledger = ledger(&reader);
    let authority = authority();
    let layers = mint_layers(&ledger, &authority);

    assert_eq!(layers.len(), 2, "one layer per admitted contributor");

    let ranks: Vec<u64> = ledger
        .contributors()
        .map(|entry| entry.rank().value() as u64)
        .collect();
    assert!(
        ranks.windows(2).all(|pair| pair[0] < pair[1]),
        "the ledger must arrive weakest first, found ranks {ranks:?}"
    );

    let expected = vec![
        ConfigLayer::new(
            &authority,
            ranks[0],
            ConfigSource::GlobalFile,
            &Secret::new(br#"{"a": 1}"#.to_vec()),
        ),
        ConfigLayer::new(
            &authority,
            ranks[1],
            ConfigSource::ProjectFile,
            &Secret::new(br#"{"b": 2}"#.to_vec()),
        ),
    ];
    assert_eq!(
        layers, expected,
        "each layer must carry its own contributor's bytes, class and rank, in ascending order"
    );
}

/// A captured document that did not decode contributes NOTHING.
///
/// The invalid contributor stays on the ledger — a caller can still see which
/// file to go fix — and never becomes a layer.
#[test]
fn an_incomplete_ledger_mints_nothing_at_all() {
    let reader = DeterministicSourceReader::new()
        .with_file(&global_path(), br#"{"a": 1}"#.to_vec())
        .with_file(&child_path(), b"{ not json".to_vec());

    let ledger = raw_ledger(&reader);
    assert_eq!(
        ledger.entries().len(),
        2,
        "both captures must be on the ledger"
    );
    assert_eq!(
        ledger.invalid().count(),
        1,
        "precondition: exactly one capture failed to decode"
    );

    // The weaker document IS admissible on its own. That is what makes this
    // fixture sharp: a bridge that minted the readable subset would produce
    // one perfectly valid-looking layer here, and the authority graph would
    // run under the global file the author had overridden.
    let admitted: Vec<_> = ledger.admitted().collect();
    assert_eq!(admitted.len(), 1);

    let refusal = match ledger.complete() {
        Ok(_) => panic!("a ledger carrying an invalid contributor must not complete"),
        Err(refusal) => refusal,
    };

    let IncompleteLedger::InvalidContributors { invalid, total, .. } = &refusal else {
        panic!("the refusal must name the invalid contributor, found {refusal:?}");
    };
    assert_eq!(*invalid, 1);
    assert_eq!(*total, 2);

    // The strongest offender is the NEAREST slot, which is the one whose
    // silent omission would have promoted the global file to authoritative.
    assert!(
        refusal.strongest_rank()
            > raw_ledger(&reader)
                .admitted()
                .map(|entry| entry.rank().value())
                .max()
                .unwrap_or_default(),
        "the undecodable document outranks the one that would have been exposed"
    );
}

/// A tree with an UNREADABLE slot mints nothing either.
///
/// Completeness spans source custody, not just decoding: a slot that could not
/// be read leaves the generation unable to say what the configuration is, and
/// minting the slots that happened to be readable would hand the graph a
/// configuration nobody has seen whole.
#[test]
fn an_obstructed_slot_mints_nothing_at_all() {
    let reader = DeterministicSourceReader::new()
        .with_file(&global_path(), br#"{"a": 1}"#.to_vec())
        .with_unreadable_file(&child_path(), std::io::ErrorKind::PermissionDenied);

    let ledger = raw_ledger(&reader);
    assert_eq!(
        ledger.invalid().count(),
        0,
        "precondition: nothing FAILED TO DECODE; the slot could not be read at all"
    );
    assert_eq!(
        ledger.admitted().count(),
        1,
        "precondition: the readable document did become a contributor"
    );

    let refusal = match ledger.complete() {
        Ok(_) => panic!("an obstructed slot must not complete"),
        Err(refusal) => refusal,
    };
    assert!(
        matches!(refusal, IncompleteLedger::Obstructed { .. }),
        "found {refusal:?}"
    );
}

/// A layer minted for one rank is not the same contribution at another.
///
/// The rank is bound into the mint, so this witness is what makes "the census
/// rank is carried through unchanged" observable: an implementation that
/// renumbered layers — compacting them, or numbering by ledger position —
/// produces a different identity for the same bytes.
#[test]
fn the_minted_identity_is_bound_to_the_census_rank() {
    let bytes = br#"{"a": 1}"#.to_vec();
    let authority = authority();

    // The same bytes at the same slot mint equal layers.
    let reader = DeterministicSourceReader::new().with_file(&child_path(), bytes.clone());
    let first = mint_layers(&ledger(&reader), &authority);
    let second = mint_layers(&ledger(&reader), &authority);
    assert_eq!(
        first, second,
        "the same document at the same rank must mint the same layer"
    );

    // The same bytes at a DIFFERENT slot mint a different layer.
    let elsewhere = DeterministicSourceReader::new().with_file(&global_path(), bytes.clone());
    let moved = mint_layers(&ledger(&elsewhere), &authority);
    assert_eq!(moved.len(), 1);
    assert_ne!(
        first, moved,
        "the same bytes at a different rank must not mint the same contribution"
    );

    // THE RANK, NOT THE CLASS. The pair above differs in `ConfigSource` too —
    // global versus project — so it stays observably different even under an
    // implementation that renumbered every layer to its position and bound the
    // wrong rank into all of them. This pair does not: both slots are ancestor
    // slots and both mint as `ProjectFile`, so the ONLY thing separating them
    // is the slot-derived rank.
    let at_parent = DeterministicSourceReader::new().with_file(&parent_path(), bytes.clone());
    let at_child = DeterministicSourceReader::new().with_file(&child_path(), bytes);
    let parent_layers = mint_layers(&ledger(&at_parent), &authority);
    let child_layers = mint_layers(&ledger(&at_child), &authority);
    assert_eq!(parent_layers.len(), 1);
    assert_eq!(child_layers.len(), 1);
    assert_ne!(
        parent_layers, child_layers,
        "two ancestor slots mint the same class, so only the rank can separate          them — equal layers here mean the rank was renumbered away"
    );
}

/// The global slot and an ancestor slot contribute as different classes.
///
/// Written as a match in the bridge rather than a default, so a slot whose
/// class was guessed would be labelled in the authority graph as something
/// nobody decided it was.
#[test]
fn the_global_slot_contributes_as_a_global_file_and_an_ancestor_as_a_project_file() {
    let global_only = DeterministicSourceReader::new().with_file(&global_path(), br#"{}"#.to_vec());
    let child_only = DeterministicSourceReader::new().with_file(&child_path(), br#"{}"#.to_vec());
    let authority = authority();

    let expected_global = ConfigLayer::new(
        &authority,
        0,
        ConfigSource::GlobalFile,
        &Secret::new(br#"{}"#.to_vec()),
    );
    let global = mint_layers(&ledger(&global_only), &authority);
    assert_eq!(
        global,
        vec![expected_global],
        "the global slot must contribute as a global file at rank 0"
    );

    let child = mint_layers(&ledger(&child_only), &authority);
    assert_eq!(child.len(), 1);
    assert_ne!(
        child, global,
        "an ancestor slot must not contribute as the global class"
    );
}

/// The minted content is the CAPTURED bytes.
///
/// Two documents that differ only in formatting decode alike. If the bridge
/// minted over a re-encoding of the decoded document they would collide; it
/// mints over what the author wrote, so they do not.
#[test]
fn the_mint_is_over_the_captured_bytes_and_not_a_re_encoding() {
    let compact = DeterministicSourceReader::new().with_file(&child_path(), br#"{"a":1}"#.to_vec());
    let spaced =
        DeterministicSourceReader::new().with_file(&child_path(), br#"{ "a" : 1 }"#.to_vec());
    let authority = authority();

    let compact_layers = mint_layers(&ledger(&compact), &authority);
    let spaced_layers = mint_layers(&ledger(&spaced), &authority);

    assert_eq!(compact_layers.len(), 1);
    assert_eq!(spaced_layers.len(), 1);
    assert_ne!(
        compact_layers, spaced_layers,
        "two byte-different documents must not mint one identity"
    );
}

/// A tree with no documents mints no layers, and does not refuse.
#[test]
fn an_empty_tree_mints_no_layers() {
    let reader = DeterministicSourceReader::new();

    assert!(mint_layers(&ledger(&reader), &authority()).is_empty());
}
