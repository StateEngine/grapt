//! The R4H-H acceptance corpus is bound to the code that actually proves it.
//!
//! `fixtures/rust-reconstruction/r4hh-bounded-http1-host/` states the bounded
//! HTTP/1 host language-neutrally. Prose alone releases nothing, so this owner
//! proves the corpus is a CLOSED typed document whose every scenario is owned by
//! a real test function in a real compiled target, and whose action log
//! references only scenarios that exist.
//!
//! ## What this validator had to fix in the one it is modelled on
//!
//! R4H-A's validator walks `#[test]` attributes, which is the right shape and
//! the wrong vocabulary for this lane: nearly every H witness is an
//! `#[tokio::test]`, several take arguments (`start_paused = true`), and all of
//! them are `async fn`. A walker that recognised only `#[test]` and only
//! `fn` would have found almost none of them and would have reported a corpus
//! of real witnesses as unowned — or, far worse, would have been quietly
//! relaxed to a substring search to make the red go away.
//! [`the_attribute_walker_understands_this_lanes_witnesses`] holds the corrected
//! walker to all four shapes AND to the prefix law it must not lose.
//!
//! A target may also name SEVERAL sources here. A's `lib` target was one file;
//! H's sealed-path witnesses are spread across the modules whose laws they
//! witness, which is where they belong — a limits witness lives beside the
//! limits.
//!
//! ## Fail-closed
//!
//! Missing, unreadable, malformed, unknown-kind, duplicate, uncovered and
//! dangling are all failures, never a skip. Every validator is a pure function
//! over already-parsed values, so the negative tests drive them with crafted
//! inputs; nothing here writes a file, because writing into the repository while
//! a mutation battery is measuring it voids the run.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The exact contract this corpus declares. Pinned, not merely non-empty.
const CONTRACT: &str = "grapt-api-bounded-http1-host-corpus/v1";
const SCHEMA_VERSION: u64 = 1;
const LANE: &str = "R4H-H";
const OWNING_CRATE: &str = "grapt-api";

/// The admitted scenario set, in order. Closed in both directions.
const ADMITTED: [&str; 34] = [
    "ready-real-http1-graceful",
    "principal-spoof-refused",
    "per-connection-request-budget",
    "global-overload-refuses-without-queueing",
    "permit-held-through-response-body",
    "connection-capacity-backpressure",
    "connection-permit-held-until-task-ends",
    "streaming-and-declared-body-bounds",
    "graceful-drain",
    "forced-close-exact-inventory",
    "accept-error-is-typed-and-bounded",
    "accept-failures-are-classified-by-kind",
    "root-task-loss-is-its-own-fact",
    "bounds-refuse-zero-naming-their-own-field",
    "checked-limits-refuse-before-a-panicking-backend",
    "permit-counts-refuse-above-the-backend-maximum",
    "receipt-counters-refuse-to-wrap",
    "deadline-authority-is-injected",
    "the-driver-reads-the-runtime-clock",
    "header-count-over-limit",
    "head-buffer-over-limit",
    "slow-partial-header-timeout",
    "unauthenticated-body-is-never-read",
    "stop-cancels-accept",
    "listener-transfer-is-linear",
    "drop-is-a-backstop-with-no-receipt",
    "direct-hyper-only",
    "one-spawn-site-under-a-permit",
    "clock-access-lives-in-one-named-adapter",
    "compile-time-seals",
    // R1 HOLD closures.
    "retained-task-set-is-bounded",
    "terminal-receipt-counts-only-live-connections",
    "no-fabricated-forced-close-backlog",
    "architecture-gate-reads-the-same-source-on-any-checkout",
];

/// The closed action vocabulary. A kind outside this set is a refusal, which is
/// what stops the log growing an untyped escape hatch.
const KINDS: [&str; 17] = [
    "host.configure",
    "host.start",
    "host.stop",
    "host.drop",
    "listener.bind",
    "client.connect",
    "client.request",
    "body.release",
    "deadline.advance",
    "deadline.fire",
    "source.accept",
    "source.scan",
    "expect.status",
    "expect.refusal",
    "expect.exit",
    "expect.route",
    "expect.source",
];

/// The owning targets and their sources, embedded at compile time.
///
/// `lib` names eleven files, not one. The sealed-path host witnesses live in the
/// `src/host_witness.rs` facade and its five direct include fragments because
/// A's fixture verification route is `#[cfg(test)]` and an integration test
/// cannot reach it; the rest live beside the law they witness. `include_str!`
/// means deleting or renaming any of them stops this target compiling.
const TARGET_SOURCES: [(&str, &str); 15] = [
    ("lib", include_str!("../src/host_witness.rs")),
    ("lib", include_str!("../src/host_witness/fixtures.rs")),
    ("lib", include_str!("../src/host_witness/http1_client.rs")),
    ("lib", include_str!("../src/host_witness/admission.rs")),
    ("lib", include_str!("../src/host_witness/shutdown.rs")),
    ("lib", include_str!("../src/host_witness/doubles.rs")),
    ("lib", include_str!("../src/admission.rs")),
    ("lib", include_str!("../src/limits.rs")),
    ("lib", include_str!("../src/deadline.rs")),
    ("lib", include_str!("../src/host.rs")),
    ("lib", include_str!("../src/host/tests.rs")),
    ("http1_hostile", include_str!("http1_hostile.rs")),
    ("shutdown_contract", include_str!("shutdown_contract.rs")),
    ("host_architecture", include_str!("host_architecture.rs")),
    ("compile_fail", include_str!("compile_fail.rs")),
];

fn corpus_dir() -> PathBuf {
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/grapt-api -> crates -> repository root
    root.pop();
    root.pop();
    root.join("fixtures/rust-reconstruction/r4hh-bounded-http1-host")
}

fn read_required(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => panic!(
            "the R4H-H corpus file {} is required and could not be read: {error}. A \
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
// Validators.
// ---------------------------------------------------------------------------

/// The pinned header. Every field is compared to an exact literal.
fn header_problems(manifest: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    let pinned: [(&str, &str); 3] = [
        ("contract", CONTRACT),
        ("lane", LANE),
        ("owning_crate", OWNING_CRATE),
    ];
    for (field, expected) in pinned {
        if manifest.get(field).and_then(Value::as_str) != Some(expected) {
            problems.push(format!(
                "{field} is {:?} but this owner admits only {expected:?}",
                manifest.get(field)
            ));
        }
    }
    if manifest.get("schema_version").and_then(Value::as_u64) != Some(SCHEMA_VERSION) {
        problems.push(format!(
            "schema_version is {:?} but this owner admits only {SCHEMA_VERSION}",
            manifest.get("schema_version")
        ));
    }
    problems
}

/// One scenario row, strictly decoded.
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

/// Every test function name in a Rust source, found by walking attributes.
///
/// Four shapes, because this lane uses all four:
///
/// * `#[test]` — the synchronous unit witnesses;
/// * `#[tokio::test]` — everything that touches a socket;
/// * `#[tokio::test(start_paused = true)]` — the deadline witnesses;
/// * `async fn` — the signature all three of the above carry.
///
/// Deliberately NOT a substring search for `fn NAME(`. A substring matches
/// inside a longer identifier, so a manifest naming a prefix of a real test
/// would be accepted for a function that does not exist.
fn test_functions(source: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut armed = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed == "#[test]"
            || trimmed == "#[tokio::test]"
            || trimmed.starts_with("#[tokio::test(")
        {
            armed = true;
            continue;
        }
        if !armed {
            continue;
        }
        // Attributes may stack between the test attribute and the signature.
        if trimmed.starts_with("#[") {
            continue;
        }
        let signature = trimmed.strip_prefix("pub ").unwrap_or(trimmed);
        let signature = signature.strip_prefix("async ").unwrap_or(signature);
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

/// Every test function across all sources embedded for one target.
fn functions_for_target(target: &str) -> Option<BTreeSet<String>> {
    let mut sources = TARGET_SOURCES
        .iter()
        .filter(|(name, _)| *name == target)
        .peekable();
    sources.peek()?;
    let mut found = BTreeSet::new();
    for (_, source) in sources {
        found.extend(test_functions(source));
    }
    Some(found)
}

/// Every scenario is owned by a real test in a target this file embeds.
fn ownership_problems(scenarios: &[Scenario]) -> Vec<String> {
    let mut problems = Vec::new();
    for scenario in scenarios {
        let Some(functions) = functions_for_target(&scenario.test_target) else {
            problems.push(format!(
                "scenario {:?} names target {:?}, which this owner does not embed; \
                 an owner it cannot see is an owner it cannot check",
                scenario.id, scenario.test_target
            ));
            continue;
        };
        if !functions.contains(&scenario.test) {
            problems.push(format!(
                "scenario {:?} names test {:?} in target {:?}, which is not a test \
                 function there",
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
        for field in ["actor", "detail"] {
            match action.get(field).and_then(Value::as_str) {
                Some(value) if !value.is_empty() => {}
                other => problems.push(format!(
                    "action {index}: {field} must be a non-empty string, found {other:?}"
                )),
            }
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
        "the R4H-H corpus manifest",
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

fn decoded_scenarios() -> Vec<Scenario> {
    match decode_scenarios(&real_manifest()) {
        Ok(scenarios) => scenarios,
        Err(problem) => panic!("the corpus scenarios did not decode: {problem}"),
    }
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
    let problems = scenario_set_problems(&decoded_scenarios());
    assert!(
        problems.is_empty(),
        "the corpus scenario set is not the admitted set: {problems:?}"
    );
}

#[test]
fn every_corpus_scenario_is_owned_by_a_real_witness() {
    let problems = ownership_problems(&decoded_scenarios());
    assert!(
        problems.is_empty(),
        "these scenarios are not owned by a real compiled witness: {problems:?}"
    );
}

#[test]
fn the_corpus_action_log_is_well_formed_and_covers_every_scenario() {
    let problems = action_problems(&decoded_scenarios(), &real_actions());
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
                "contract": "grapt-api-bounded-http1-host-corpus/v2",
                "schema_version": 1, "lane": LANE, "owning_crate": OWNING_CRATE
            }),
        ),
        (
            "another lane's contract id",
            serde_json::json!({
                "contract": "grapt-api-auth-acceptance-corpus/v1",
                "schema_version": 1, "lane": LANE, "owning_crate": OWNING_CRATE
            }),
        ),
        (
            "a bumped schema version",
            serde_json::json!({
                "contract": CONTRACT, "schema_version": 2,
                "lane": LANE, "owning_crate": OWNING_CRATE
            }),
        ),
        (
            "another lane",
            serde_json::json!({
                "contract": CONTRACT, "schema_version": 1,
                "lane": "R4H-A", "owning_crate": OWNING_CRATE
            }),
        ),
        (
            "another owning crate",
            serde_json::json!({
                "contract": CONTRACT, "schema_version": 1,
                "lane": LANE, "owning_crate": "grapt-location"
            }),
        ),
        (
            "a missing contract",
            serde_json::json!({
                "schema_version": 1, "lane": LANE, "owning_crate": OWNING_CRATE
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

    // The same-file control.
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

    let mut short = full.clone();
    short.pop();
    assert!(
        !scenario_set_problems(&short).is_empty(),
        "a missing scenario was admitted"
    );

    // An extra row — the direction an "every admitted id is present" check
    // misses entirely.
    let mut long = full.clone();
    long.push(scenario("an-invented-scenario"));
    assert!(
        !scenario_set_problems(&long).is_empty(),
        "an extra scenario was admitted"
    );

    let mut swapped = full.clone();
    swapped.swap(0, 1);
    assert!(
        !scenario_set_problems(&swapped).is_empty(),
        "a reordering was admitted"
    );

    let mut renamed = full.clone();
    renamed[8] = scenario("forced-close-is-graceful-really");
    assert!(
        !scenario_set_problems(&renamed).is_empty(),
        "a renamed scenario was admitted"
    );

    assert!(scenario_set_problems(&full).is_empty());
}

#[test]
fn a_scenario_owned_by_nothing_real_is_refused() {
    let owned = |target: &str, test: &str| {
        vec![Scenario {
            id: "graceful-drain".to_owned(),
            claim: "c".to_owned(),
            test_target: target.to_owned(),
            test: test.to_owned(),
        }]
    };

    assert!(
        !ownership_problems(&owned("lib", "a_test_that_was_never_written")).is_empty(),
        "a scenario naming a nonexistent test was admitted"
    );
    assert!(
        !ownership_problems(&owned("some_other_target", "whatever")).is_empty(),
        "a scenario naming an unembedded target was admitted"
    );
    assert!(
        !ownership_problems(&owned(
            "http1_hostile",
            "a_stop_with_a_completing_response_drains_gracefully"
        ))
        .is_empty(),
        "a scenario naming a real test in the wrong target was admitted"
    );
    // A PREFIX of a real name. This is the case a substring search accepts and
    // attribute walking refuses.
    assert!(
        !ownership_problems(&owned("lib", "a_stop_with_a_completing_response")).is_empty(),
        "a prefix of a real test name was admitted; ownership must not be a \
         substring search"
    );
    // An ordinary helper that carries no test attribute at all.
    assert!(
        !ownership_problems(&owned("lib", "two_seat_boundary")).is_empty(),
        "a plain helper function was accepted as a witness owner"
    );

    // The same-file control: a real witness in its real target passes.
    assert!(
        ownership_problems(&owned(
            "lib",
            "a_stop_with_a_completing_response_drains_gracefully"
        ))
        .is_empty(),
        "a real witness was refused, which would make every case above pass for \
         the wrong reason"
    );
}

#[test]
fn a_malformed_action_log_is_refused() {
    let scenarios = vec![Scenario {
        id: "graceful-drain".to_owned(),
        claim: "c".to_owned(),
        test_target: "lib".to_owned(),
        test: "a_stop_with_a_completing_response_drains_gracefully".to_owned(),
    }];
    let good = serde_json::json!({
        "ordinal": 1, "scenario": "graceful-drain",
        "actor": "server", "kind": "host.stop", "detail": "d"
    });

    let cases = [
        ("an empty log leaves the scenario uncovered", Vec::new()),
        (
            "a non-contiguous ordinal",
            vec![serde_json::json!({
                "ordinal": 2, "scenario": "graceful-drain",
                "actor": "server", "kind": "host.stop", "detail": "d"
            })],
        ),
        (
            "a dangling scenario reference",
            vec![serde_json::json!({
                "ordinal": 1, "scenario": "a-scenario-that-is-not-declared",
                "actor": "server", "kind": "host.stop", "detail": "d"
            })],
        ),
        (
            "a kind outside the closed vocabulary",
            vec![serde_json::json!({
                "ordinal": 1, "scenario": "graceful-drain",
                "actor": "server", "kind": "host.pretend", "detail": "d"
            })],
        ),
        (
            "an empty actor",
            vec![serde_json::json!({
                "ordinal": 1, "scenario": "graceful-drain",
                "actor": "", "kind": "host.stop", "detail": "d"
            })],
        ),
        (
            "a missing detail",
            vec![serde_json::json!({
                "ordinal": 1, "scenario": "graceful-drain",
                "actor": "server", "kind": "host.stop"
            })],
        ),
    ];
    for (route, actions) in cases {
        assert!(
            !action_problems(&scenarios, &actions).is_empty(),
            "the {route} route was admitted"
        );
    }

    assert!(action_problems(&scenarios, &[good]).is_empty());
}

#[test]
fn the_attribute_walker_understands_this_lanes_witnesses() {
    // All four shapes this lane uses, plus the prefix law the walker must not
    // lose while gaining them. Written as one source so the walker is exercised
    // exactly as it runs.
    let source = "\
#[test]\n\
fn a_synchronous_witness() {}\n\
#[tokio::test]\n\
async fn an_async_witness() {}\n\
#[tokio::test(start_paused = true)]\n\
async fn a_paused_clock_witness() {}\n\
#[tokio::test]\n\
#[ignore]\n\
async fn stacked_attributes_still_count() {}\n\
async fn a_plain_async_helper() {}\n\
fn a_plain_helper() {}\n";
    let found = test_functions(source);

    for expected in [
        "a_synchronous_witness",
        "an_async_witness",
        "a_paused_clock_witness",
        "stacked_attributes_still_count",
    ] {
        assert!(
            found.contains(expected),
            "the walker missed {expected:?}, which is one of the four shapes this \
             lane's witnesses take"
        );
    }
    for rejected in ["a_plain_async_helper", "a_plain_helper", "an_async"] {
        assert!(
            !found.contains(rejected),
            "the walker accepted {rejected:?}, which is either not a witness or is \
             a prefix of one"
        );
    }
    assert_eq!(found.len(), 4);
}

#[test]
fn a_target_may_own_several_sources_and_an_unknown_target_owns_none() {
    // `lib` is eleven files. A lookup that returned only the first would silently
    // orphan every witness in the other ten, and the ownership check would
    // start reporting real witnesses as missing — the exact pressure that
    // tempts an author to relax it to a substring search.
    let Some(functions) = functions_for_target("lib") else {
        panic!("the lib target embeds no source");
    };
    let across_files = [
        (
            "host_witness/shutdown.rs",
            "a_stop_with_a_completing_response_drains_gracefully",
        ),
        (
            "admission.rs",
            "global_admission_refuses_immediately_rather_than_queueing",
        ),
        ("limits.rs", "every_bound_refuses_zero_naming_its_own_field"),
        (
            "deadline.rs",
            "the_timer_delegates_every_method_to_its_driver",
        ),
        (
            "host/tests.rs",
            "accept_failures_are_classified_by_kind_and_never_by_text",
        ),
    ];
    for (file, witness) in across_files {
        assert!(
            functions.contains(witness),
            "the lib target lost {witness:?} from {file}"
        );
    }

    assert!(
        functions_for_target("a_target_that_is_not_embedded").is_none(),
        "an unembedded target must be reported as absent rather than as empty; \
         empty would make every scenario naming it fail for the wrong reason"
    );
}
