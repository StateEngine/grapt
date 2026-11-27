//! The R4H-A acceptance corpus is bound to the code that actually proves it.
//!
//! `fixtures/rust-reconstruction/r4-api-auth-foundation/` states the transport
//! trust boundary language-neutrally. Prose alone releases nothing, so this
//! owner proves the corpus is a CLOSED typed document whose every scenario is
//! owned by a real `#[test]` in a real compiled target, and whose action log
//! references only scenarios that exist.
//!
//! ## Fail-closed, because the alternative has already been measured
//!
//! R4H-L's cold review turned three corpus corruptions green against a
//! validator that required only non-empty strings, never pinned the admitted
//! contract, and reduced ownership to a substring search. This lane pays that
//! lesson forward instead of re-learning it:
//!
//! * the contract, schema version, lane and owning crate are PINNED to exact
//!   literals, so a fabricated contract id is a refusal rather than a
//!   non-empty string;
//! * the scenario set is closed in BOTH directions against [`ADMITTED`] — a
//!   missing row and an extra row are both failures, and so is a reordering;
//! * ownership is resolved by walking `#[test]` attributes in the target's
//!   embedded source, never by substring. `fn a_displaced_lease_is_not_current`
//!   contains `a_displaced_lease` as a substring, and a substring check would
//!   accept a manifest naming the shorter name for a test that does not exist.
//!
//! Every guard fails closed: missing, unreadable, malformed, unknown-kind,
//! duplicate, uncovered and dangling are all failures, never a skip. That
//! matters more than usual here, because this corpus is validated by nothing
//! else — this file is the only executable thing between it and rot.
//!
//! ## How the binding is structural
//!
//! Each owning target's source is embedded with `include_str!`, so deleting or
//! renaming one stops this target compiling. Within that source, owners are
//! recognised by attribute walking, so renaming a witness function turns this
//! red rather than silently orphaning a scenario.
//!
//! ## The validators are pure functions
//!
//! Every check below is a function over already-parsed values, and the negative
//! tests drive those functions with crafted inputs. Nothing writes a file:
//! writing into the repository while a mutation battery is measuring it voids
//! the run.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The exact contract this corpus declares. Pinned, not merely non-empty.
const CONTRACT: &str = "grapt-api-auth-acceptance-corpus/v1";
const SCHEMA_VERSION: u64 = 1;
const LANE: &str = "R4H-A";
const OWNING_CRATE: &str = "grapt-api";

/// The admitted scenario set, in order. Closed in both directions.
const ADMITTED: [&str; 13] = [
    "sealed-mapping-mints-principal",
    "dto-recipient-never-selects",
    "dto-recipient-disagreement-refused",
    "inert-client-id-is-refused",
    "unauthenticated-never-reaches-application",
    "authentication-is-not-authorization",
    "revocation-invalidates-live-principal",
    "directory-bound-refuses-oversize",
    "takeover-displaces-prior-lease",
    "takeover-slot-bound-refuses",
    "router-composition-is-deterministic",
    "router-refuses-before-serving",
    "errors-do-not-echo",
];

/// The closed action vocabulary. A kind outside this set is a refusal, which is
/// what stops the log growing an untyped escape hatch.
const KINDS: [&str; 12] = [
    "directory.configure",
    "subject.verify",
    "principal.mint",
    "principal.observe",
    "authorize",
    "directory.revoke",
    "takeover.acquire",
    "takeover.observe",
    "router.register",
    "router.seal",
    "dto.decode",
    "expect.refusal",
];

/// The owning targets, and their sources embedded at compile time.
///
/// `lib` is `src/witness.rs`, which is `#[cfg(test)]` and therefore compiles
/// into the library's own test binary. That is where the sealed-path witnesses
/// must live: the mint they exercise is crate-private, so an integration test —
/// a separate crate — cannot reach it.
const TARGET_SOURCES: [(&str, &str); 2] = [
    ("lib", include_str!("../src/witness.rs")),
    ("router_contract", include_str!("router_contract.rs")),
];

fn corpus_dir() -> PathBuf {
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/grapt-api -> crates -> repository root
    root.pop();
    root.pop();
    root.join("fixtures/rust-reconstruction/r4-api-auth-foundation")
}

/// Read a required corpus file, refusing absence and unreadability alike.
fn read_required(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => panic!(
            "the R4H-A corpus file {} is required and could not be read: {error}. A \
             corpus runner with no corpus is a broken gate, and a gate that skips \
             silently is worse than one that fails.",
            path.display()
        ),
    }
}

fn parse_json(label: &str, text: &str) -> Value {
    match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => panic!("{label} is not valid JSON: {error}"),
    }
}

// ---------------------------------------------------------------------------
// Validators. Pure functions over parsed values, so the negative tests can
// drive them with crafted inputs and never touch the filesystem.
// ---------------------------------------------------------------------------

/// The pinned header. Every field is compared to an exact literal.
fn header_problems(manifest: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    if manifest.get("contract").and_then(Value::as_str) != Some(CONTRACT) {
        problems.push(format!(
            "contract is {:?} but this owner admits only {CONTRACT:?}",
            manifest.get("contract")
        ));
    }
    if manifest.get("schema_version").and_then(Value::as_u64) != Some(SCHEMA_VERSION) {
        problems.push(format!(
            "schema_version is {:?} but this owner admits only {SCHEMA_VERSION}",
            manifest.get("schema_version")
        ));
    }
    if manifest.get("lane").and_then(Value::as_str) != Some(LANE) {
        problems.push(format!(
            "lane is {:?} but this owner admits only {LANE:?}",
            manifest.get("lane")
        ));
    }
    if manifest.get("owning_crate").and_then(Value::as_str) != Some(OWNING_CRATE) {
        problems.push(format!(
            "owning_crate is {:?} but this owner admits only {OWNING_CRATE:?}",
            manifest.get("owning_crate")
        ));
    }
    problems
}

/// One scenario row, strictly decoded. Every field is required and must be a
/// non-empty string; a missing or wrongly typed field is a refusal, never a
/// default.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Scenario {
    id: String,
    claim: String,
    test_target: String,
    test: String,
}

fn decode_scenarios(manifest: &Value) -> Result<Vec<Scenario>, String> {
    let Some(rows) = manifest.get("scenarios").and_then(Value::as_array) else {
        return Err("the manifest declares no `scenarios` array".to_owned());
    };
    let mut scenarios = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let field = |name: &str| -> Result<String, String> {
            match row.get(name).and_then(Value::as_str) {
                Some(value) if !value.is_empty() => Ok(value.to_owned()),
                other => Err(format!(
                    "scenario {index}: `{name}` must be a non-empty string, found {other:?}"
                )),
            }
        };
        scenarios.push(Scenario {
            id: field("id")?,
            claim: field("claim")?,
            test_target: field("test_target")?,
            test: field("test")?,
        });
    }
    Ok(scenarios)
}

/// The scenario set, closed in BOTH directions and order-checked.
fn scenario_set_problems(scenarios: &[Scenario]) -> Vec<String> {
    let mut problems = Vec::new();
    let declared: Vec<&str> = scenarios.iter().map(|s| s.id.as_str()).collect();
    if declared.len() != ADMITTED.len() {
        problems.push(format!(
            "the corpus declares {} scenarios, but exactly {} are admitted",
            declared.len(),
            ADMITTED.len()
        ));
    }
    for (position, admitted) in ADMITTED.iter().enumerate() {
        match declared.get(position) {
            Some(found) if found == admitted => {}
            Some(found) => problems.push(format!(
                "scenario {found:?} is not the admitted scenario {admitted:?}; the \
                 corpus must declare exactly the admitted scenarios, in order"
            )),
            None => problems.push(format!("the admitted scenario {admitted:?} is missing")),
        }
    }
    let unique: BTreeSet<&str> = declared.iter().copied().collect();
    if unique.len() != declared.len() {
        problems.push("the corpus declares a duplicate scenario id".to_owned());
    }
    problems
}

/// Every `#[test]` function name in a Rust source, found by walking attributes.
///
/// Deliberately NOT a substring search for `fn NAME(`. A substring matches
/// inside a longer identifier, so a manifest naming a prefix of a real test
/// would be accepted for a function that does not exist.
fn test_functions(source: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut armed = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed == "#[test]" {
            armed = true;
            continue;
        }
        if !armed {
            continue;
        }
        // Attributes may stack between `#[test]` and the signature.
        if trimmed.starts_with("#[") {
            continue;
        }
        let signature = trimmed.strip_prefix("pub ").unwrap_or(trimmed);
        if let Some(rest) = signature.strip_prefix("fn ") {
            let name: String = rest
                .chars()
                .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
                .collect();
            if !name.is_empty() {
                found.insert(name);
            }
        }
        armed = false;
    }
    found
}

/// Every scenario is owned by a real `#[test]` in a target this file embeds.
fn ownership_problems(scenarios: &[Scenario]) -> Vec<String> {
    let mut problems = Vec::new();
    for scenario in scenarios {
        let Some((_, source)) = TARGET_SOURCES
            .iter()
            .find(|(name, _)| *name == scenario.test_target)
        else {
            problems.push(format!(
                "scenario {:?} names target {:?}, which this owner does not embed; \
                 an owner it cannot see is an owner it cannot check",
                scenario.id, scenario.test_target
            ));
            continue;
        };
        if !test_functions(source).contains(&scenario.test) {
            problems.push(format!(
                "scenario {:?} names test {:?} in target {:?}, which is not a \
                 #[test] function there",
                scenario.id, scenario.test, scenario.test_target
            ));
        }
    }
    problems
}

/// The action log: contiguous ordinals, known kinds, no dangling scenario, and
/// every declared scenario covered.
fn action_problems(scenarios: &[Scenario], actions: &[Value]) -> Vec<String> {
    let mut problems = Vec::new();
    let declared: BTreeSet<&str> = scenarios.iter().map(|s| s.id.as_str()).collect();
    let mut covered: BTreeSet<String> = BTreeSet::new();

    if actions.is_empty() {
        problems.push("the action log is empty".to_owned());
    }
    for (index, action) in actions.iter().enumerate() {
        let expected = u64::try_from(index + 1).unwrap_or(u64::MAX);
        match action.get("ordinal").and_then(Value::as_u64) {
            Some(found) if found == expected => {}
            other => problems.push(format!(
                "action {index}: ordinal is {other:?} but must be {expected}; a \
                 non-contiguous log has lost a row without saying so"
            )),
        }
        match action.get("scenario").and_then(Value::as_str) {
            Some(scenario) if declared.contains(scenario) => {
                covered.insert(scenario.to_owned());
            }
            other => problems.push(format!(
                "action {index}: scenario {other:?} is not declared by the manifest"
            )),
        }
        match action.get("kind").and_then(Value::as_str) {
            Some(kind) if KINDS.contains(&kind) => {}
            other => problems.push(format!(
                "action {index}: kind {other:?} is outside the closed vocabulary"
            )),
        }
        match action.get("actor").and_then(Value::as_str) {
            Some(actor) if !actor.is_empty() => {}
            other => problems.push(format!(
                "action {index}: actor must be a non-empty string, found {other:?}"
            )),
        }
        match action.get("detail").and_then(Value::as_str) {
            Some(detail) if !detail.is_empty() => {}
            other => problems.push(format!(
                "action {index}: detail must be a non-empty string, found {other:?}"
            )),
        }
    }
    for scenario in &declared {
        if !covered.contains(*scenario) {
            problems.push(format!(
                "scenario {scenario:?} is declared but no action exercises it; an \
                 unexercised claim is prose"
            ));
        }
    }
    problems
}

fn real_manifest() -> Value {
    parse_json(
        "the R4H-A corpus manifest",
        &read_required(&corpus_dir().join("manifest.json")),
    )
}

fn real_actions() -> Vec<Value> {
    let text = read_required(&corpus_dir().join("actions.jsonl"));
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .enumerate()
        .map(|(index, line)| parse_json(&format!("action line {}", index + 1), line))
        .collect()
}

// ---------------------------------------------------------------------------
// The corpus itself.
// ---------------------------------------------------------------------------

#[test]
fn the_corpus_declares_the_admitted_contract() {
    let problems = header_problems(&real_manifest());
    assert!(
        problems.is_empty(),
        "the corpus header does not match this owner: {problems:?}"
    );
}

#[test]
fn the_corpus_scenario_set_is_closed_in_both_directions() {
    let manifest = real_manifest();
    let scenarios = match decode_scenarios(&manifest) {
        Ok(scenarios) => scenarios,
        Err(problem) => panic!("the corpus scenarios did not decode: {problem}"),
    };
    let problems = scenario_set_problems(&scenarios);
    assert!(
        problems.is_empty(),
        "the corpus scenario set is not the admitted set: {problems:?}"
    );
}

#[test]
fn every_corpus_scenario_is_owned_by_a_real_witness() {
    let manifest = real_manifest();
    let scenarios = match decode_scenarios(&manifest) {
        Ok(scenarios) => scenarios,
        Err(problem) => panic!("the corpus scenarios did not decode: {problem}"),
    };
    let problems = ownership_problems(&scenarios);
    assert!(
        problems.is_empty(),
        "these scenarios are not owned by a real compiled witness: {problems:?}"
    );
}

#[test]
fn the_corpus_action_log_is_well_formed_and_covers_every_scenario() {
    let manifest = real_manifest();
    let scenarios = match decode_scenarios(&manifest) {
        Ok(scenarios) => scenarios,
        Err(problem) => panic!("the corpus scenarios did not decode: {problem}"),
    };
    let problems = action_problems(&scenarios, &real_actions());
    assert!(
        problems.is_empty(),
        "the corpus action log is malformed: {problems:?}"
    );
}

// ---------------------------------------------------------------------------
// The negative half. Each validator above is driven with a crafted corruption
// and must refuse it. Without these, every check above could be vacuous.
// ---------------------------------------------------------------------------

#[test]
fn a_fabricated_contract_identity_is_refused() {
    let cases = [
        (
            "a fabricated contract id",
            serde_json::json!({
                "contract": "grapt-api-auth-acceptance-corpus/v2",
                "schema_version": 1, "lane": "R4H-A", "owning_crate": "grapt-api"
            }),
        ),
        (
            "another lane's contract id",
            serde_json::json!({
                "contract": "grapt-location-acceptance-corpus/v1",
                "schema_version": 1, "lane": "R4H-A", "owning_crate": "grapt-api"
            }),
        ),
        (
            "a bumped schema version",
            serde_json::json!({
                "contract": CONTRACT, "schema_version": 2,
                "lane": "R4H-A", "owning_crate": "grapt-api"
            }),
        ),
        (
            "another lane",
            serde_json::json!({
                "contract": CONTRACT, "schema_version": 1,
                "lane": "R4H-L", "owning_crate": "grapt-api"
            }),
        ),
        (
            "another owning crate",
            serde_json::json!({
                "contract": CONTRACT, "schema_version": 1,
                "lane": "R4H-A", "owning_crate": "grapt-location"
            }),
        ),
        (
            "a missing contract",
            serde_json::json!({
                "schema_version": 1, "lane": "R4H-A", "owning_crate": "grapt-api"
            }),
        ),
    ];
    for (route, manifest) in cases {
        assert!(
            !header_problems(&manifest).is_empty(),
            "the {route} route was admitted; the header must be pinned, not merely \
             present"
        );
    }

    // The same-file control: the real header passes, so the cases above are not
    // passing because the checker refuses everything.
    assert!(header_problems(&real_manifest()).is_empty());
}

#[test]
fn a_corrupted_scenario_set_is_refused() {
    let scenario = |id: &str| Scenario {
        id: id.to_owned(),
        claim: "c".to_owned(),
        test_target: "lib".to_owned(),
        test: "t".to_owned(),
    };
    let full: Vec<Scenario> = ADMITTED.iter().map(|id| scenario(id)).collect();

    // A missing row.
    let mut short = full.clone();
    short.pop();
    assert!(
        !scenario_set_problems(&short).is_empty(),
        "a missing scenario was admitted"
    );

    // An extra row — the direction a "every admitted id is present" check misses.
    let mut long = full.clone();
    long.push(scenario("an-invented-scenario"));
    assert!(
        !scenario_set_problems(&long).is_empty(),
        "an extra scenario was admitted"
    );

    // A reordering. The set is the same; the order is not.
    let mut swapped = full.clone();
    swapped.swap(0, 1);
    assert!(
        !scenario_set_problems(&swapped).is_empty(),
        "a reordering was admitted"
    );

    // A renamed row.
    let mut renamed = full.clone();
    renamed[3] = scenario("inert-client-id-is-allowed");
    assert!(
        !scenario_set_problems(&renamed).is_empty(),
        "a renamed scenario was admitted"
    );

    // The same-file control.
    assert!(scenario_set_problems(&full).is_empty());
}

#[test]
fn a_scenario_owned_by_nothing_real_is_refused() {
    let owned = |target: &str, test: &str| {
        vec![Scenario {
            id: "sealed-mapping-mints-principal".to_owned(),
            claim: "c".to_owned(),
            test_target: target.to_owned(),
            test: test.to_owned(),
        }]
    };

    // A function that does not exist.
    assert!(
        !ownership_problems(&owned("lib", "a_test_that_was_never_written")).is_empty(),
        "a scenario naming a nonexistent test was admitted"
    );

    // A target this owner does not embed — an owner it cannot see is an owner it
    // cannot check.
    assert!(
        !ownership_problems(&owned("some_other_target", "whatever")).is_empty(),
        "a scenario naming an unembedded target was admitted"
    );

    // A real function in the WRONG target.
    assert!(
        !ownership_problems(&owned(
            "router_contract",
            "a_verified_subject_resolves_server_side_and_mints_the_sealed_principal"
        ))
        .is_empty(),
        "a scenario naming a real test in the wrong target was admitted"
    );

    // A PREFIX of a real function name. This is the case a substring search
    // accepts and attribute walking refuses.
    assert!(
        !ownership_problems(&owned("lib", "a_displaced_lease")).is_empty(),
        "a prefix of a real test name was admitted; ownership must not be a \
         substring search"
    );

    // An ordinary helper that is not a #[test] at all.
    assert!(
        !ownership_problems(&owned("lib", "two_seat_boundary")).is_empty(),
        "a plain helper function was accepted as a witness owner"
    );

    // The same-file control: a real #[test] in its real target passes.
    assert!(
        ownership_problems(&owned("lib", "a_displaced_lease_is_not_current")).is_empty(),
        "a real witness was refused, which would make every case above pass for \
         the wrong reason"
    );
}

#[test]
fn a_malformed_action_log_is_refused() {
    let scenarios = vec![Scenario {
        id: "errors-do-not-echo".to_owned(),
        claim: "c".to_owned(),
        test_target: "lib".to_owned(),
        test: "a_refusal_never_renders_what_it_observed".to_owned(),
    }];
    let good = serde_json::json!({
        "ordinal": 1, "scenario": "errors-do-not-echo",
        "actor": "observer", "kind": "expect.refusal", "detail": "d"
    });

    let cases = [
        ("an empty log leaves the scenario uncovered", Vec::new()),
        (
            "a non-contiguous ordinal",
            vec![serde_json::json!({
                "ordinal": 2, "scenario": "errors-do-not-echo",
                "actor": "observer", "kind": "expect.refusal", "detail": "d"
            })],
        ),
        (
            "a dangling scenario reference",
            vec![serde_json::json!({
                "ordinal": 1, "scenario": "a-scenario-that-is-not-declared",
                "actor": "observer", "kind": "expect.refusal", "detail": "d"
            })],
        ),
        (
            "a kind outside the closed vocabulary",
            vec![serde_json::json!({
                "ordinal": 1, "scenario": "errors-do-not-echo",
                "actor": "observer", "kind": "expect.success", "detail": "d"
            })],
        ),
        (
            "an empty actor",
            vec![serde_json::json!({
                "ordinal": 1, "scenario": "errors-do-not-echo",
                "actor": "", "kind": "expect.refusal", "detail": "d"
            })],
        ),
        (
            "a missing detail",
            vec![serde_json::json!({
                "ordinal": 1, "scenario": "errors-do-not-echo",
                "actor": "observer", "kind": "expect.refusal"
            })],
        ),
    ];
    for (route, actions) in cases {
        assert!(
            !action_problems(&scenarios, &actions).is_empty(),
            "the {route} route was admitted"
        );
    }

    // The same-file control.
    assert!(action_problems(&scenarios, &[good]).is_empty());
}

#[test]
fn the_attribute_walker_does_not_match_inside_a_name() {
    // The unit-level statement of the prefix law, so the property is asserted
    // directly and not only through the ownership check above.
    let source = "#[test]\nfn a_displaced_lease_is_not_current() {}\n\
                  fn a_plain_helper() {}\n\
                  #[test]\n#[ignore]\nfn stacked_attributes_still_count() {}\n";
    let found = test_functions(source);

    assert!(found.contains("a_displaced_lease_is_not_current"));
    assert!(
        !found.contains("a_displaced_lease"),
        "a prefix must not be reported as a test name"
    );
    assert!(
        !found.contains("a_plain_helper"),
        "a function without #[test] is not a witness owner"
    );
    assert!(
        found.contains("stacked_attributes_still_count"),
        "attributes may stack between #[test] and the signature"
    );
    assert_eq!(found.len(), 2);
}
