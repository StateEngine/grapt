//! The structural gate over review blocker 1.
//!
//! # Why a source-scanning gate and not just types
//!
//! The type discipline this crate now carries is real: `SessionWireId` is the
//! only session identity, the compiler enforces it at every call site, and the
//! ordinary suite exercises it. So what does scanning source text add?
//!
//! It catches the RE-INTRODUCTION, which is the failure mode that actually
//! happened. The previous revision did not fail because someone disbelieved in
//! typed identities — it failed because a `String` field, a `starts_with`
//! check and a `.to_string()` at a boundary each looked locally reasonable, and
//! nothing anywhere said "this crate does not do that". Types stop a mistake;
//! this stops a DRIFT, because a future author who reaches for
//! `session_id: String` gets a red test naming the law instead of a clean
//! compile.
//!
//! It is also the only instrument that can witness an ABSENCE. There is no
//! runtime assertion for "no map is keyed on a string session id", because the
//! call that would prove it is the defect itself.
//!
//! # Line endings
//!
//! This repository carries no `.gitattributes`, so a checkout under
//! `core.autocrlf=true` hands these files CRLF while the authoring tree has LF.
//! A source-scanning gate that compared raw bytes would then be green on one
//! machine and red on another for reasons having nothing to do with its law.
//! Every read here is normalized to LF first, and the normalization is the
//! reason this gate is portable rather than a lane tax.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The largest source file this gate will read, in bytes.
///
/// A bound rather than an unbounded read, because this gate is itself subject
/// to the bounded-everything law it exists next to.
const MAX_SOURCE_BYTES: u64 = 1_048_576;

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Strip whole-line comments, leaving the CODE.
///
/// A source gate that scanned prose would punish the one thing this crate most
/// needs to keep doing: explaining, at the site, what the old defect was. Every
/// module corrected in this commit names `starts_with("ses")` and
/// `session_id: String` in its documentation precisely so the next reader
/// understands why the current shape is what it is — and the first run of this
/// gate went red on exactly those sentences.
///
/// So the scanner reads code. Only whole-line comments are removed, not
/// trailing ones: a trailing `//` cannot be told from a `//` inside a string
/// literal without a real lexer, and a gate that guessed would fail in the
/// direction that hides defects. Erring toward scanning MORE text is the safe
/// direction for an absence gate.
fn strip_line_comments(text: &str) -> String {
    text.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Read one source file, normalized to LF.
fn read_normalized(path: &Path) -> String {
    let metadata = std::fs::metadata(path)
        .unwrap_or_else(|error| panic!("source must be stattable at {path:?}: {error}"));
    assert!(
        metadata.len() <= MAX_SOURCE_BYTES,
        "{path:?} exceeds the {MAX_SOURCE_BYTES}-byte read bound"
    );
    let bytes = std::fs::read(path)
        .unwrap_or_else(|error| panic!("source must be readable at {path:?}: {error}"));
    let text = String::from_utf8(bytes)
        .unwrap_or_else(|error| panic!("source must be UTF-8 at {path:?}: {error}"));
    text.replace("\r\n", "\n")
}

/// Every PRODUCTION source file of this crate, keyed by file name.
///
/// Test modules are excluded deliberately: a witness legitimately spells a
/// literal identity, and including them would force every assertion below to
/// carry an exception list that would eventually swallow a real defect.
fn production_sources() -> BTreeMap<String, String> {
    let source_root = crate_root().join("src");
    let mut found = BTreeMap::new();
    let mut stack = vec![source_root.clone()];
    while let Some(directory) = stack.pop() {
        let entries = std::fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("src must be readable at {directory:?}: {error}"));
        for entry in entries {
            let entry = entry.unwrap_or_else(|error| panic!("src entry must be readable: {error}"));
            let path = entry.path();
            let kind = entry
                .file_type()
                .unwrap_or_else(|error| panic!("src entry must be stattable: {error}"));
            if kind.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_else(|| panic!("source file name must be UTF-8: {path:?}"))
                .to_owned();
            // WITNESSES ARE NOT PRODUCTION. The exclusion used to be the file
            // NAME `tests.rs`, which stopped being the whole answer the moment
            // an oversized test module was split into `tests/<subject>.rs`
            // siblings: the same witnesses, suddenly read as production code.
            // The rule was always "inside the tests module", so that is what it
            // now asks.
            if name == "tests.rs" {
                continue;
            }
            if directory
                .components()
                .any(|component| component.as_os_str() == "tests")
            {
                continue;
            }
            let relative = path
                .strip_prefix(&source_root)
                .unwrap_or_else(|error| panic!("source must live under src: {error}"))
                .to_string_lossy()
                .replace('\\', "/");
            found.insert(relative, strip_line_comments(&read_normalized(&path)));
        }
    }
    assert!(
        found.len() >= 12,
        "the production source set collapsed to {} files; this gate would be vacuous",
        found.len()
    );
    found
}

#[test]
fn no_production_source_flattens_a_session_identity_into_a_string() {
    // Each pattern below is a SPELLING THAT ACTUALLY EXISTED in the reviewed
    // candidate, not a hypothetical. That matters: a gate assembled from
    // imagined defects tends to miss the real one, and every entry here can be
    // pointed at a line review cited.
    let banned: &[(&str, &str)] = &[
        (
            "session_id: String",
            "`PermissionAsk.session_id` and `EventEnvelope.session_id` were both this",
        ),
        (
            "session_id: Option<String>",
            "the envelope's optional session was this",
        ),
        (
            "aggregate_id: String",
            "`DurableRef.aggregate_id` was this, so a watermark was keyed on text",
        ),
        (
            "selected: Option<String>",
            "the client's selection was this",
        ),
        (
            "BTreeMap<String, SessionView>",
            "the client's projection map was this",
        ),
        (
            "BTreeMap<String, DoubleSession>",
            "the double's session store was this",
        ),
        (
            "BTreeMap<String, Vec<PendingPermission>>",
            "the double's permission store was this",
        ),
        (
            "BTreeMap<String, u64>",
            "the cursor's watermark map was this",
        ),
        (
            "aggregate: String",
            "`GapReason::SequenceGap` carried the aggregate as text",
        ),
    ];
    let sources = production_sources();
    for (pattern, why) in banned {
        for (name, body) in &sources {
            assert!(
                !body.contains(pattern),
                "{name} contains `{pattern}`, flattening a typed identity back into a \
                 string. This is review blocker 1 returning: {why}."
            );
        }
    }
}

#[test]
fn no_production_source_accepts_an_identity_by_prefix() {
    // The specific defect: `starts_with("ses")` admitted `sesABC`, which
    // `grapt_types::SessionId` refuses. The constants that carried those
    // prefixes are deleted, so this also proves they did not come back.
    let banned = [
        "SESSION_ID_PREFIX",
        "EVENT_ID_PREFIX",
        "starts_with(\"ses",
        "starts_with(\"evt",
        "starts_with(EVENT",
        "starts_with(SESSION",
    ];
    let sources = production_sources();
    for pattern in banned {
        for (name, body) in &sources {
            assert!(
                !body.contains(pattern),
                "{name} contains `{pattern}`: identity is decided by parsing through \
                 `grapt_types`, never by a prefix test. A prefix test says nothing about \
                 the rest of the value, which is how `ses_` with an empty suffix passed."
            );
        }
    }
}

#[test]
fn spelling_an_identity_is_confined_to_rendering_and_encoding() {
    // `Spelled::spelled` is the deliberate escape hatch from a typed identity
    // to text. An escape hatch nobody bounds becomes the ordinary route, so the
    // set of modules allowed to use it is pinned HERE and is small.
    //
    // The allowance is by ROLE, not by convenience: `view.rs` paints characters
    // on a terminal, `double.rs` and `wire.rs` encode JSON, and `http.rs` writes
    // a request line. None of them makes a decision from the text.
    let allowed = [
        "view.rs",
        "double.rs",
        "wire.rs",
        "http.rs",
        "contract.rs",
        "projection.rs",
        "transport.rs",
    ];
    // The allowance is by MODULE, not by file: `wire/catalogs.rs` is part of
    // the module that encodes, and splitting an oversized file into subject
    // modules must not silently move code out of a role it still has — nor
    // silently INTO one, which is why the root is taken rather than any
    // component matching.
    for (name, body) in production_sources() {
        let module = match name.split_once('/') {
            Some((root, _)) => format!("{root}.rs"),
            None => name.clone(),
        };
        if allowed.contains(&module.as_str()) {
            continue;
        }
        assert!(
            !body.contains(".spelled()"),
            "{name} spells an identity as text. Only {allowed:?} may, because only they \
             render or encode. Everywhere else, compare the identities."
        );
    }
}

#[test]
fn there_is_exactly_one_route_from_a_typed_identity_to_text() {
    // WITHOUT THIS, THE GATE ABOVE HAS A HOLE BIG ENOUGH TO WALK THROUGH.
    //
    // `spelling_an_identity_is_confined_to_rendering_and_encoding` bans
    // `.spelled()` outside the modules that render or encode. But `.spelled()`
    // is only the POLITE route: `id.as_inner().as_str()` produces exactly the
    // same `String` and was not banned anywhere. Three production modules were
    // in fact using it, so the confinement test was guarding a door beside an
    // open window.
    //
    // THE FIRST FIX OF THAT WAS ALSO WRONG, and this is the correction review
    // returned. It banned `as_inner` — the spelling it had been shown — and
    // stopped. `WireId<T>` declares `as_inner` and `into_inner` in ONE `impl`
    // block three lines apart, and `WireId` is `Clone`, so
    // `id.clone().into_inner().as_str()` compiles anywhere. The test whose NAME
    // is "exactly one route" reported ok, and so did the whole 191-test suite.
    //
    // A ban-list over spellings is unbounded and you will always be one
    // spelling short. So this does not ban a spelling: it reads the PRODUCER,
    // enumerates every accessor `WireId` actually declares, and refuses if that
    // set is not the set this crate has reasoned about. A future third accessor
    // reddens here and someone decides deliberately, rather than silently
    // reopening the route.
    let accessors = wire_id_accessors();
    let expected: BTreeSet<&str> = ["as_inner", "into_inner", "parse"].into_iter().collect();
    let observed: BTreeSet<&str> = accessors.iter().map(String::as_str).collect();
    assert_eq!(
        observed, expected,
        "`WireId`'s public surface changed. This crate confines the accessors that \
         reach the raw identity, and it can only do that over a set it knows. Decide \
         deliberately whether the new member reaches the value, then update this \
         expectation AND the ban below — in that order."
    );

    // Of that set, these two REACH THE VALUE. `parse` constructs and is safe
    // anywhere; the other two hand out the inner identity and are confined to
    // the module that defines the `Spelled` seam.
    let value_reaching = ["as_inner", "into_inner"];
    for (name, body) in production_sources() {
        // The `wire` MODULE, not the file: it was split by subject when it
        // passed the workspace module ceiling, and every part of it is still
        // the seam that defines `Spelled`. Confinement follows the module that
        // owns the seam, or a decomposition would quietly widen it.
        let module = match name.split_once('/') {
            Some((root, _)) => format!("{root}.rs"),
            None => name.clone(),
        };
        if module == "wire.rs" {
            continue;
        }
        for accessor in value_reaching {
            assert!(
                !body.contains(&format!(".{accessor}(")),
                "{name} reaches through `{accessor}` to a raw identity. \
                 `Spelled::spelled` is the one admitted route from a typed identity to \
                 text, and it lives in wire.rs. Every accessor that reaches the value \
                 has to be confined, not just the one somebody was shown."
            );
        }
    }
}

/// Every public accessor `WireId` declares, read from the crate that OWNS it.
///
/// Derived rather than mirrored. A hardcoded list here would be a second copy of
/// `grapt-api`'s public surface, free to drift away from it silently — and
/// drifting silently is exactly how `into_inner` came to be unguarded. Reading
/// the producer means the day someone adds a third accessor, this crate finds
/// out from the compiler's own source of truth instead of from a reviewer.
fn wire_id_accessors() -> BTreeSet<String> {
    let path = crate_root()
        .join("../grapt-api/src/u3a/codec.rs")
        .canonicalize()
        .unwrap_or_else(|error| panic!("the WireId producer must be locatable: {error}"));
    let source = strip_line_comments(&read_normalized(&path));
    let mut found = BTreeSet::new();
    let mut inside = false;
    let mut depth = 0usize;
    for line in source.lines() {
        let trimmed = line.trim();
        if !inside && trimmed.starts_with("impl") && trimmed.contains("WireId<T>") {
            // Only inherent impls. `impl ... Serialize for WireId<T>` is a trait
            // implementation, whose methods are not an accessor surface a caller
            // reaches for, and counting them would make this set churn on every
            // derive.
            if trimmed.contains(" for ") {
                continue;
            }
            inside = true;
            depth = 0;
        }
        if inside {
            depth += line.matches('{').count();
            if let Some(rest) = trimmed.strip_prefix("pub fn ") {
                let name: String = rest
                    .chars()
                    .take_while(|character| character.is_alphanumeric() || *character == '_')
                    .collect();
                if !name.is_empty() {
                    found.insert(name);
                }
            }
            depth = depth.saturating_sub(line.matches('}').count());
            if depth == 0 && line.contains('}') {
                inside = false;
            }
        }
    }
    assert!(
        !found.is_empty(),
        "no `WireId` inherent accessor was found; the scanner is not reading the \
         producer, and an empty set would make the confinement below vacuous"
    );
    found
}

#[test]
fn the_banned_patterns_are_findable_so_this_gate_is_not_vacuous() {
    // ANTI-VACUITY. Every assertion above is an ABSENCE, and an absence test
    // passes just as cheerfully when its subject is unreachable — a typo in a
    // pattern, a source set that silently collapsed to nothing, a scanner
    // reading the wrong directory. So this proves the instrument works by
    // finding a string that IS present.
    let sources = production_sources();
    let hits = sources
        .values()
        .filter(|body| body.contains("SessionWireId"))
        .count();
    assert!(
        hits >= 5,
        "only {hits} production sources mention `SessionWireId`; the scanner is \
         probably not reading what it thinks it is"
    );
    // And the scanner really does see the text of the file it names.
    let client = sources
        .get("client.rs")
        .unwrap_or_else(|| panic!("client.rs must be in the production source set"));
    assert!(
        client.contains("fn ingest("),
        "the scanner is not reading client.rs's real contents"
    );
    // And the comment stripper really strips:  documents the old
    // defect by name, so if this string survived, the scanner would be reading
    // prose and every absence assertion above would be measuring the wrong text.
    assert!(
        !client.contains("handed the body straight to"),
        "whole-line comments must be stripped before scanning"
    );
}
