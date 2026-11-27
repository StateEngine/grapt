//! A generation advances exactly once per complete load, and a refused reload
//! changes nothing.
//!
//! Every witness here runs against a DECLARED tree, so what a reload sees is a
//! function of the fixture rather than of the machine the gate runs on. A
//! reload is handed a DIFFERENT reader from the load before it — that is the
//! whole point of the type — so each witness names two trees and the change
//! between them is the input under test.
//!
//! The load-bearing witnesses in this file are
//! [`a_refused_reload_leaves_the_generation_unchanged`] and
//! [`a_refused_reload_leaves_the_previous_configuration_in_force`]. An
//! implementation that advanced the counter on the way to returning `Err`, or
//! that installed a half-built configuration before discovering it was
//! incomplete, passes every witness that only ever reloads a tree of
//! well-formed documents. Both failures are invisible from the success path
//! and both are silent in production: the caller gets its `Err`, logs it, and
//! carries on under a configuration that is no longer the one it thinks it has.
//!
//! # Why no witness here refuses at DISCOVERY
//!
//! [`ReloadRefusal::Discovery`] is unreachable on a reload, and stating that is
//! better than leaving a reader to wonder why the arm is untested. A
//! `DiscoveryRefusal` is raised by path formation and slot ranking, both of
//! which are functions of the PLAN and the SOURCE BOUNDS alone — never of what
//! a reader observes. A session fixes both at [`ConfigSession::load`], so a
//! session that exists at all has already proved its plan walks. The arm is
//! carried because `load` shares `load_once` with `reload` and the FIRST load
//! genuinely can refuse there; it is exercised through `load` in
//! [`a_first_load_that_cannot_walk_its_plan_never_becomes_a_session`].
//!
//! The two refusals a reload CAN reach are both here, and both are reachable
//! only because the reader changed: an admission that exceeds the document
//! bound, and a ledger that is not complete.

use crate::support;

use grapt_config::{
    ConfigBounds, ConfigFileName, ConfigSession, ConfigSourcePath, DeterministicSourceReader,
    DiscoveryPlan, Generation, IncompleteLedger, LedgerRefusal, MaxPathBytes, ReloadRefusal,
    SourceBounds,
};
use support::custom_bounds;
use support::source_support::{abs, custom_source_bounds, file_name, identity, try_source_bounds};

const CONFIG: &str = "config.jsonc";

fn plan() -> DiscoveryPlan {
    DiscoveryPlan::new(identity(&["project", "child"]), None, file_name(CONFIG))
}

fn child_path() -> std::path::PathBuf {
    abs(&["project", "child", CONFIG])
}

fn parent_path() -> std::path::PathBuf {
    abs(&["project", CONFIG])
}

/// Generous decode bounds. The document CAP is the parameter, because one
/// witness reloads into a tree that exceeds it and every other witness must
/// never trip it.
fn bounds(documents: usize) -> ConfigBounds {
    custom_bounds(1024 * 1024, 32, documents, 32, 10_000)
}

/// Source bounds generous enough that the WALK never refuses. A discovery that
/// refused first would prove nothing about a reload, for the reason this file's
/// module documentation gives.
fn source_bounds() -> SourceBounds {
    custom_source_bounds(1024 * 1024, 2, 64, 4096)
}

/// A reader holding one document at the nearest slot.
fn tree(content: &[u8]) -> DeterministicSourceReader {
    DeterministicSourceReader::new().with_file(&child_path(), content.to_vec())
}

/// A session over `tree(content)`, with a document cap of 64.
fn session(reader: &DeterministicSourceReader) -> ConfigSession {
    session_with(reader, 64)
}

fn session_with(reader: &DeterministicSourceReader, documents: usize) -> ConfigSession {
    match ConfigSession::load(plan(), reader, bounds(documents), source_bounds()) {
        Ok(session) => session,
        Err(refusal) => panic!("the first load was refused: {refusal}"),
    }
}

/// The bytes every contributor of the configuration in force decoded from,
/// weakest first.
///
/// The held configuration is compared through its contributors' CANONICAL
/// BYTES rather than through the decoded documents, because the bytes are what
/// the author wrote: two different documents that decode to the same value are
/// the same configuration, and a witness that could not tell a reload from a
/// no-op would be no witness at all.
fn held_bytes(session: &ConfigSession) -> Vec<Vec<u8>> {
    session
        .configuration()
        .contributors()
        .map(|entry| match entry.bytes() {
            Some(bytes) => bytes.to_vec(),
            // UNREACHABLE: every contributor of a complete ledger kept its
            // bytes. Written as a total match because `unwrap` is denied here.
            None => panic!("a contributor of a complete ledger carried no bytes"),
        })
        .collect()
}

fn reload(session: &mut ConfigSession, reader: &DeterministicSourceReader) -> Generation {
    match session.reload(reader) {
        Ok(generation) => generation,
        Err(refusal) => panic!("the reload was refused: {refusal}"),
    }
}

// ---------------------------------------------------------------------------
// The first load
// ---------------------------------------------------------------------------

/// A session begins at the FIRST generation, not at zero.
///
/// The paired positive for every "unchanged" witness below: without it, an
/// implementation whose generation never moved at all would keep all of them
/// green.
#[test]
fn the_first_complete_load_holds_the_first_generation() {
    let reader = tree(br#"{"a": 1}"#);

    let session = session(&reader);

    assert_eq!(
        session.generation().value(),
        1,
        "the first complete load is generation one, so that zero is free for a \
         caller to mean `I have never loaded`"
    );
    assert_eq!(
        held_bytes(&session),
        vec![br#"{"a": 1}"#.to_vec()],
        "the session holds what the first load found"
    );
}

/// A first load that cannot produce a complete configuration produces no
/// session at all.
///
/// There is no state in which a caller holds a session whose generation numbers
/// a configuration it does not have, because the constructor is the only way in
/// and it refuses whole.
#[test]
fn a_first_load_that_is_incomplete_never_becomes_a_session() {
    let reader = tree(b"{");

    let refusal = match ConfigSession::load(plan(), &reader, bounds(64), source_bounds()) {
        Ok(session) => panic!(
            "an undecodable tree produced a session at generation {}",
            session.generation()
        ),
        Err(refusal) => refusal,
    };

    assert!(
        matches!(refusal, ReloadRefusal::Incomplete(_)),
        "an undecodable document refuses as incomplete, not as something else: {refusal}"
    );
}

/// The `Discovery` arm is reachable, on the FIRST load, and only there.
///
/// Witnessed with a path bound too small to form the slot path. This is the arm
/// this file's documentation says a reload can never reach; proving it is
/// reachable SOMEWHERE is what makes that claim a scoping statement rather than
/// a cover for an untested branch.
#[test]
fn a_first_load_that_cannot_walk_its_plan_never_becomes_a_session() {
    let reader = tree(br#"{"a": 1}"#);

    // A path allowance large enough to hold the start directory and too small
    // to hold the file path built from it.
    let Ok(tight) = try_source_bounds(1024 * 1024, 2, 64, 24) else {
        panic!("the tight source bounds were themselves refused");
    };
    let Ok(path_bytes) = MaxPathBytes::try_new(4096) else {
        panic!("path byte bound refused");
    };
    let Ok(start) = ConfigSourcePath::new(&abs(&["project", "child"]), path_bytes) else {
        panic!("start path refused");
    };
    let Some(name) = ConfigFileName::new(CONFIG) else {
        panic!("config file name refused");
    };

    let refusal = match ConfigSession::load(
        DiscoveryPlan::new(start, None, name),
        &reader,
        bounds(64),
        tight,
    ) {
        Ok(_) => panic!("a plan that cannot be walked produced a session"),
        Err(refusal) => refusal,
    };

    assert!(
        matches!(refusal, ReloadRefusal::Discovery(_)),
        "a path that cannot be formed refuses at discovery: {refusal}"
    );
}

// ---------------------------------------------------------------------------
// A complete reload advances exactly once
// ---------------------------------------------------------------------------

/// One complete reload advances the generation by EXACTLY one.
///
/// Asserted as an equality against a computed successor rather than as
/// `after > before`. An ordering assertion is satisfied by any advance,
/// including two, and "the generation moved by an amount no load produced" is
/// precisely the failure a consumer that steps generations cannot recover from.
#[test]
fn a_complete_reload_advances_the_generation_exactly_once() {
    let first = tree(br#"{"a": 1}"#);
    let mut session = session(&first);
    let before = session.generation().value();

    let second = tree(br#"{"a": 2}"#);
    let returned = reload(&mut session, &second);

    assert_eq!(
        session.generation().value(),
        before + 1,
        "exactly one advance for one complete load — not two, and not none"
    );
    assert_eq!(
        returned,
        session.generation(),
        "the generation handed back is the one the session now holds"
    );
}

/// Repeated complete reloads advance once EACH, and never repeat a number.
///
/// The single-step witness above cannot separate "advances once" from "advances
/// to a fixed second value", and it cannot see a counter that advances once and
/// then stalls. Four loads, four numbers, all distinct and consecutive.
#[test]
fn every_complete_reload_advances_once_and_no_generation_repeats() {
    let first = tree(br#"{"a": 1}"#);
    let mut session = session(&first);

    let mut seen = vec![session.generation().value()];
    for step in 2..=4u64 {
        let next = tree(format!(r#"{{"a": {step}}}"#).as_bytes());
        seen.push(reload(&mut session, &next).value());
    }

    assert_eq!(
        seen,
        vec![1, 2, 3, 4],
        "three complete reloads over a first load produce four consecutive \
         generations, each used once"
    );
}

/// A complete reload installs the configuration it just loaded.
///
/// The counterpart to the "changes nothing" witnesses, and the one that stops
/// them from being satisfied by an implementation that changes nothing EVER.
/// An advanced generation over a stale configuration is the worst of the
/// available failures: every consumer is told something changed, re-derives,
/// and re-derives from exactly the bytes it already had.
#[test]
fn a_complete_reload_installs_the_newly_loaded_configuration() {
    let first = tree(br#"{"a": 1}"#);
    let mut session = session(&first);
    assert_eq!(
        held_bytes(&session),
        vec![br#"{"a": 1}"#.to_vec()],
        "precondition: the session holds the first tree"
    );

    let second = tree(br#"{"a": 2}"#);
    reload(&mut session, &second);

    assert_eq!(
        held_bytes(&session),
        vec![br#"{"a": 2}"#.to_vec()],
        "the configuration in force is the one the advancing load read, not the \
         one it replaced"
    );
}

/// A complete reload of an UNCHANGED tree still advances.
///
/// A generation numbers a LOAD, not a change. Comparing contents to decide
/// whether to advance would require this crate to decide what "the same
/// configuration" means, which is the same domain judgement it declines to make
/// when it declines to merge — and it would make the number stop being a thing
/// a caller can compare, because two different loads could share one.
#[test]
fn a_complete_reload_of_an_unchanged_tree_still_advances() {
    let first = tree(br#"{"a": 1}"#);
    let mut session = session(&first);

    let identical = tree(br#"{"a": 1}"#);
    reload(&mut session, &identical);

    assert_eq!(
        session.generation().value(),
        2,
        "the generation numbers the load, not the difference"
    );
}

// ---------------------------------------------------------------------------
// A refused reload changes nothing
// ---------------------------------------------------------------------------

/// A reload refused for an INCOMPLETE ledger leaves the generation where it was.
#[test]
fn a_refused_reload_leaves_the_generation_unchanged() {
    let first = tree(br#"{"a": 1}"#);
    let mut session = session(&first);
    let before = session.generation();

    let broken = tree(b"{");
    let refusal = match session.reload(&broken) {
        Ok(generation) => panic!("an undecodable tree advanced to generation {generation}"),
        Err(refusal) => refusal,
    };

    assert!(
        matches!(refusal.cause(), ReloadRefusal::Incomplete(_)),
        "precondition: this reload was refused for incompleteness: {refusal}"
    );
    assert_eq!(
        session.generation(),
        before,
        "a refusal is not a version: the counter did not move"
    );
}

/// A reload refused for an incomplete ledger leaves the CONFIGURATION in force.
///
/// Asserted AFTER the refusal, over the held bytes, rather than by asserting
/// only that an `Err` came back. An implementation that returned the correct
/// `Err` while clobbering the held state on the way out satisfies every
/// assertion about the return value and fails this one.
#[test]
fn a_refused_reload_leaves_the_previous_configuration_in_force() {
    let first = tree(br#"{"a": 1}"#);
    let mut session = session(&first);
    let before = held_bytes(&session);

    let broken = tree(b"{");
    if let Ok(generation) = session.reload(&broken) {
        panic!("an undecodable tree advanced to generation {generation}");
    }

    assert_eq!(
        held_bytes(&session),
        before,
        "the configuration the program runs under is the last one that loaded \
         completely"
    );
    assert_eq!(
        held_bytes(&session),
        vec![br#"{"a": 1}"#.to_vec()],
        "and it is the FIRST tree specifically, not merely something unchanged"
    );
}

/// A reload refused for an OBSTRUCTED slot changes nothing either.
///
/// A second refusal shape, because obstruction refuses at a different place:
/// the slot could not be read at all, so no document was ever judged. An
/// implementation that reset its state before deciding which refusal applied
/// would pass the undecodable witness and fail this one.
#[test]
fn a_reload_refused_for_an_obstructed_slot_changes_nothing() {
    let first = tree(br#"{"a": 1}"#);
    let mut session = session(&first);
    let before = held_bytes(&session);
    let generation = session.generation();

    let obstructed = DeterministicSourceReader::new()
        .with_unreadable_file(&child_path(), std::io::ErrorKind::PermissionDenied);
    let refusal = match session.reload(&obstructed) {
        Ok(advanced) => panic!("an unreadable slot advanced to generation {advanced}"),
        Err(refusal) => refusal,
    };

    assert!(
        matches!(
            refusal.cause(),
            ReloadRefusal::Incomplete(IncompleteLedger::Obstructed { .. })
        ),
        "a slot that could not be read refuses as obstructed: {refusal}"
    );
    assert_eq!(session.generation(), generation, "the counter did not move");
    assert_eq!(
        held_bytes(&session),
        before,
        "the configuration did not move"
    );
}

/// A reload refused at ADMISSION — over the document bound — changes nothing.
///
/// The third and last refusal a reload can reach, and the only one that refuses
/// before a ledger exists at all. The session is built with a cap of one and
/// reloads into a tree holding two documents.
#[test]
fn a_reload_refused_over_the_document_bound_changes_nothing() {
    let first = tree(br#"{"a": 1}"#);
    let mut session = session_with(&first, 1);
    let before = held_bytes(&session);
    let generation = session.generation();

    let crowded = DeterministicSourceReader::new()
        .with_file(&child_path(), br#"{"a": 2}"#.to_vec())
        .with_file(&parent_path(), br#"{"b": 3}"#.to_vec());
    let refusal = match session.reload(&crowded) {
        Ok(advanced) => panic!("a tree over the document bound advanced to {advanced}"),
        Err(refusal) => refusal,
    };

    assert!(
        matches!(
            refusal.cause(),
            ReloadRefusal::Ledger(LedgerRefusal::TooManyDocuments { .. })
        ),
        "a census over the cap refuses at admission: {refusal}"
    );
    assert_eq!(session.generation(), generation, "the counter did not move");
    assert_eq!(
        held_bytes(&session),
        before,
        "the configuration did not move"
    );
}

/// A refusal REPORTS the generation still in force.
///
/// The number in the refusal is the number a caller will log, and a caller that
/// logged the generation the reload WOULD have produced would tell its reader
/// the program is running under a configuration that was never loaded. Asserted
/// against the session's own generation read after the refusal, so the two can
/// never drift apart silently.
#[test]
fn a_refusal_reports_the_generation_still_in_force() {
    let first = tree(br#"{"a": 1}"#);
    let mut session = session(&first);
    let second = tree(br#"{"a": 2}"#);
    reload(&mut session, &second);

    let broken = tree(b"{");
    let refusal = match session.reload(&broken) {
        Ok(generation) => panic!("an undecodable tree advanced to generation {generation}"),
        Err(refusal) => refusal,
    };

    assert_eq!(
        refusal.held(),
        session.generation(),
        "the refusal names the generation the session still holds"
    );
    assert_eq!(
        refusal.held().value(),
        2,
        "and that is the second generation specifically — the one the last \
         complete load produced, never the third the refused load would have"
    );
}

/// A refused reload does not poison the session: the next complete reload
/// advances by one from the generation that survived the refusal.
///
/// Without this, an implementation could satisfy every witness above by
/// recording the skipped advance somewhere and applying it later. The
/// generation after a refusal-then-success is the second, not the third.
#[test]
fn a_session_that_refused_a_reload_advances_normally_afterwards() {
    let first = tree(br#"{"a": 1}"#);
    let mut session = session(&first);

    let broken = tree(b"{");
    if let Ok(generation) = session.reload(&broken) {
        panic!("an undecodable tree advanced to generation {generation}");
    }

    let good = tree(br#"{"a": 2}"#);
    let advanced = reload(&mut session, &good);

    assert_eq!(
        advanced.value(),
        2,
        "the refused attempt consumed no number: the next complete load is the \
         second generation, not the third"
    );
    assert_eq!(
        held_bytes(&session),
        vec![br#"{"a": 2}"#.to_vec()],
        "and it installed the tree it read"
    );
}

// ---------------------------------------------------------------------------
// What a session fixes, and what it does not
// ---------------------------------------------------------------------------

/// The plan is fixed for the life of the session.
///
/// A reload answers the SAME question as the load before it. If the plan could
/// change, two generations would number answers to two different questions and
/// comparing them would mean nothing.
#[test]
fn a_session_reloads_under_the_plan_it_was_built_with() {
    let first = tree(br#"{"a": 1}"#);
    let mut session = session(&first);
    let expected = plan();

    let second = tree(br#"{"a": 2}"#);
    reload(&mut session, &second);

    assert_eq!(
        session.plan(),
        &expected,
        "the plan a session reloads under is the one it was constructed with"
    );
}

/// A tree that holds NO configuration document is a complete answer, and it
/// advances the generation like any other.
///
/// Every slot was looked at and none held a file. That is something the author
/// arranged, not a failure to observe the world, and a session that refused it
/// would be unable to represent an unconfigured machine at all.
#[test]
fn reloading_into_a_tree_with_no_documents_is_complete_and_advances() {
    let first = tree(br#"{"a": 1}"#);
    let mut session = session(&first);

    let empty = DeterministicSourceReader::new();
    reload(&mut session, &empty);

    assert_eq!(
        session.generation().value(),
        2,
        "an empty answer is an answer, and it is numbered"
    );
    assert!(
        held_bytes(&session).is_empty(),
        "the configuration in force carries no contributors"
    );
}
