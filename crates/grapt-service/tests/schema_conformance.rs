//! Validates this lane's corpus manifest against the **actual ratified schema
//! file**, resolved through the frozen-fixture seam.
//!
//! # Why this test exists
//!
//! The R1 review found the manifest declaring the schema's `$id` URL where the
//! ratified schema pins `"$schema": {"const": "../oracle-receipt-manifest.schema.json"}`.
//! It survived because the corpus owner pinned the *same wrong literal* and
//! asserted equality with it: corpus and owner agreed with each other and both
//! diverged from the contract they named. A self-consistent parser cannot detect
//! that, by construction.
//!
//! So the ratified file is the oracle here. Nothing in this crate restates what
//! the schema says; every expectation is read out of the schema at run time.
//!
//! # Where the schema comes from
//!
//! `test-support/fixtures.rs` — the landed seam — resolves it from
//! `SECODE_FIXTURES`, else `REMOTE_COMPUTE_ARTIFACT_ROOT/fixtures/rust-reconstruction`
//! on the governed host, else the Windows compatibility root. `fixture_path`
//! asserts the file exists, so an absent schema is a loud failure and never a
//! skip. No copy of the schema is vendored into this repository.
//!
//! # The evaluator's bound, and why it cannot under-validate
//!
//! This is not a general JSON Schema implementation. It implements the keywords
//! the ratified schema actually uses, and **panics on any keyword it meets and
//! does not implement**. Under-validation is therefore impossible: if the schema
//! gains a keyword this evaluator does not know, the test fails loudly instead of
//! quietly checking less than it claims. That is the same failure class the R1
//! review found, so it is refused structurally rather than by care.
//!
//! That guarantee is **witnessed**, not merely asserted here. The R2 closure
//! review deleted the refusal and the suite stayed green, because the ratified
//! schema happens to use no unimplemented keyword — so the arm that makes the
//! promise true was unreachable and nothing scored it. A stated limit a reviewer
//! can mutate through is a survivor, not a note. Three tests now drive the
//! refusals directly against synthetic schemas, which is the only way to reach
//! an arm the real schema cannot: `an_unimplemented_assertion_keyword_is_refused_rather_than_skipped`,
//! `an_unknown_type_value_is_refused_rather_than_assumed`, and
//! `every_type_arm_separates_a_matching_instance_from_a_violating_one`. The
//! synthetic schemas are test inputs only — the evaluator itself is unchanged and
//! stays bounded.

use std::panic::AssertUnwindSafe;
use std::path::Path;

use regex_lite::Regex;
use serde_json::{json, Value};

#[path = "../../../test-support/fixtures.rs"]
mod fixtures;

/// Keywords that carry no assertion and are skipped deliberately.
const ANNOTATIONS: &[&str] = &["$id", "$schema", "title", "description"];

fn corpus_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/rust-reconstruction/r4-desktop-attachment")
}

fn read(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(body) => body,
        Err(error) => panic!("cannot read {}: {error}", path.display()),
    }
}

fn parse(body: &str, what: &str) -> Value {
    match serde_json::from_str(body) {
        Ok(value) => value,
        Err(error) => panic!("{what} is not valid JSON: {error}"),
    }
}

fn type_matches(instance: &Value, expected: &str) -> bool {
    match expected {
        "object" => instance.is_object(),
        "array" => instance.is_array(),
        "string" => instance.is_string(),
        "integer" => instance.as_i64().is_some() || instance.as_u64().is_some(),
        "number" => instance.is_number(),
        "boolean" => instance.is_boolean(),
        "null" => instance.is_null(),
        other => panic!("schema uses an unknown `type` value {other:?}"),
    }
}

/// Validate `instance` against `schema`, appending human-readable failures.
///
/// Returns nothing; `errors` empty means valid. Every keyword encountered is
/// either implemented here or panics — see the module note.
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn validate(instance: &Value, schema: &Value, path: &str, errors: &mut Vec<String>) {
    let Some(map) = schema.as_object() else {
        panic!("schema at {path} is not an object");
    };

    for (keyword, argument) in map {
        if ANNOTATIONS.contains(&keyword.as_str()) {
            continue;
        }
        match keyword.as_str() {
            "type" => {
                let ok = match argument {
                    Value::String(name) => type_matches(instance, name),
                    Value::Array(names) => names.iter().any(|name| match name.as_str() {
                        Some(name) => type_matches(instance, name),
                        None => panic!("schema `type` array at {path} holds a non-string"),
                    }),
                    other => panic!("schema `type` at {path} is neither string nor array: {other}"),
                };
                if !ok {
                    errors.push(format!("{path}: type must be {argument}"));
                }
            }
            "const" => {
                if instance != argument {
                    errors.push(format!(
                        "{path}: must equal the constant {argument}, found {instance}"
                    ));
                }
            }
            "required" => {
                let Some(names) = argument.as_array() else {
                    panic!("schema `required` at {path} is not an array");
                };
                if let Some(object) = instance.as_object() {
                    for name in names {
                        let Some(name) = name.as_str() else {
                            panic!("schema `required` at {path} holds a non-string");
                        };
                        if !object.contains_key(name) {
                            errors.push(format!("{path}: missing required property `{name}`"));
                        }
                    }
                }
            }
            "properties" => {
                let Some(properties) = argument.as_object() else {
                    panic!("schema `properties` at {path} is not an object");
                };
                if let Some(object) = instance.as_object() {
                    for (name, subschema) in properties {
                        if let Some(child) = object.get(name) {
                            validate(child, subschema, &format!("{path}/{name}"), errors);
                        }
                    }
                }
            }
            "additionalProperties" => {
                if argument == &Value::Bool(false) {
                    if let Some(object) = instance.as_object() {
                        let declared = map
                            .get("properties")
                            .and_then(Value::as_object)
                            .map(|properties| properties.keys().cloned().collect::<Vec<_>>())
                            .unwrap_or_default();
                        for name in object.keys() {
                            if !declared.contains(name) {
                                errors.push(format!("{path}: property `{name}` is not permitted"));
                            }
                        }
                    }
                } else {
                    panic!("schema `additionalProperties` at {path} is not the literal false");
                }
            }
            "items" => {
                if let Some(array) = instance.as_array() {
                    // With `prefixItems` present, `items` governs the tail.
                    let offset = map
                        .get("prefixItems")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0);
                    for (index, element) in array.iter().enumerate().skip(offset) {
                        validate(element, argument, &format!("{path}[{index}]"), errors);
                    }
                }
            }
            "prefixItems" => {
                let Some(prefix) = argument.as_array() else {
                    panic!("schema `prefixItems` at {path} is not an array");
                };
                if let Some(array) = instance.as_array() {
                    for (index, subschema) in prefix.iter().enumerate() {
                        if let Some(element) = array.get(index) {
                            validate(element, subschema, &format!("{path}[{index}]"), errors);
                        }
                    }
                }
            }
            "minItems" | "maxItems" => {
                let Some(bound) = argument.as_u64() else {
                    panic!("schema `{keyword}` at {path} is not an integer");
                };
                if let Some(array) = instance.as_array() {
                    let length = array.len() as u64;
                    let bad = if keyword == "minItems" {
                        length < bound
                    } else {
                        length > bound
                    };
                    if bad {
                        errors.push(format!("{path}: {keyword} is {bound}, found {length}"));
                    }
                }
            }
            "minLength" => {
                let Some(bound) = argument.as_u64() else {
                    panic!("schema `minLength` at {path} is not an integer");
                };
                if let Some(text) = instance.as_str() {
                    if (text.chars().count() as u64) < bound {
                        errors.push(format!(
                            "{path}: minLength is {bound}, found {}",
                            text.len()
                        ));
                    }
                }
            }
            "pattern" => {
                let Some(source) = argument.as_str() else {
                    panic!("schema `pattern` at {path} is not a string");
                };
                let expression = match Regex::new(source) {
                    Ok(expression) => expression,
                    Err(error) => panic!("schema `pattern` at {path} does not compile: {error}"),
                };
                if let Some(text) = instance.as_str() {
                    if !expression.is_match(text) {
                        errors.push(format!("{path}: {text:?} does not match /{source}/"));
                    }
                }
            }
            "oneOf" => {
                let Some(branches) = argument.as_array() else {
                    panic!("schema `oneOf` at {path} is not an array");
                };
                let matched = branches
                    .iter()
                    .filter(|branch| {
                        let mut branch_errors = Vec::new();
                        validate(instance, branch, path, &mut branch_errors);
                        branch_errors.is_empty()
                    })
                    .count();
                if matched != 1 {
                    errors.push(format!(
                        "{path}: exactly one `oneOf` branch must match, {matched} did"
                    ));
                }
            }
            "not" => {
                let mut inner = Vec::new();
                validate(instance, argument, path, &mut inner);
                if inner.is_empty() {
                    errors.push(format!("{path}: must NOT match the forbidden subschema"));
                }
            }
            other => panic!(
                "the ratified schema uses `{other}` at {path}, which this evaluator does not \
                 implement. Refusing to report a pass it did not check: implement the keyword \
                 rather than ignoring it."
            ),
        }
    }
}

fn ratified_schema() -> Value {
    let path = fixtures::fixture_path(Path::new("oracle-receipt-manifest.schema.json"));
    parse(&read(&path), "the ratified oracle-receipt schema")
}

#[test]
fn the_manifest_validates_against_the_actual_ratified_schema() {
    let schema = ratified_schema();
    let manifest = parse(
        &read(&corpus_dir().join("manifest.json")),
        "r4-desktop-attachment/manifest.json",
    );

    let mut errors = Vec::new();
    validate(&manifest, &schema, "", &mut errors);
    assert!(
        errors.is_empty(),
        "the corpus manifest does not validate against the ratified schema:\n  {}",
        errors.join("\n  ")
    );
}

/// R4D-2A's corpus, which is a separate closed directory from R4D-1's.
///
/// A closed-corpus validator claims its whole fixture directory, so the two
/// lanes keep one directory each rather than sharing one. This function is the
/// only reason this file mentions the second lane at all: the bounded evaluator
/// above is the crate's single JSON Schema implementation, and giving the new
/// corpus its own copy would put two evaluators in one crate — the exact
/// competing-truth shape the R1 review found.
fn differential_corpus_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/rust-reconstruction/r4-desktop-differential-contract")
}

#[test]
fn the_differential_contract_manifest_validates_against_the_actual_ratified_schema() {
    let schema = ratified_schema();
    let manifest = parse(
        &read(&differential_corpus_dir().join("manifest.json")),
        "r4-desktop-differential-contract/manifest.json",
    );

    let mut errors = Vec::new();
    validate(&manifest, &schema, "", &mut errors);
    assert!(
        errors.is_empty(),
        "the differential contract manifest does not validate against the ratified schema:\n  {}",
        errors.join("\n  ")
    );
}

/// R4D-2C owns a closed corpus directory rather than appending rows to another
/// lane's evidence surface.
fn controller_corpus_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/rust-reconstruction/r4-service-controller")
}

#[test]
fn the_service_controller_manifest_validates_against_the_actual_ratified_schema() {
    let schema = ratified_schema();
    let manifest = parse(
        &read(&controller_corpus_dir().join("manifest.json")),
        "r4-service-controller/manifest.json",
    );
    let mut errors = Vec::new();
    validate(&manifest, &schema, "", &mut errors);
    assert!(
        errors.is_empty(),
        "the service controller manifest does not validate against the ratified schema:\n  {}",
        errors.join("\n  ")
    );
}

#[test]
fn every_path_the_service_controller_manifest_declares_exists() {
    let dir = controller_corpus_dir();
    let manifest = parse(
        &read(&dir.join("manifest.json")),
        "r4-service-controller/manifest.json",
    );
    let paths = match manifest.get("paths") {
        Some(paths) => paths,
        None => panic!("the manifest declares no `paths` object"),
    };
    let mut declared: Vec<String> = Vec::new();
    if let Some(actions) = paths.get("actions").and_then(Value::as_str) {
        declared.push(actions.to_owned());
    }
    match paths.get("expected").and_then(Value::as_object) {
        Some(expected) => {
            for (key, value) in expected {
                match value.as_str() {
                    Some(path) => declared.push(path.to_owned()),
                    None => panic!("declared expected path `{key}` is not a string"),
                }
            }
        }
        None => panic!("the manifest declares no `paths.expected` object"),
    }
    assert!(
        declared.len() >= 4,
        "expected actions plus three expected surfaces, found {}: {declared:?}",
        declared.len()
    );
    for relative in declared {
        let path = dir.join(&relative);
        assert!(
            path.is_file(),
            "the manifest declares `{relative}`, absent at {}",
            path.display()
        );
    }
}

#[test]
fn the_evaluator_detects_a_violation_of_the_schema_it_read() {
    // The same-file control. Without it, `the_manifest_validates_…` could be
    // passing because the evaluator reports success on everything — which is
    // exactly how the R1 defect survived its own owner.
    let schema = ratified_schema();
    let mut manifest = parse(
        &read(&corpus_dir().join("manifest.json")),
        "r4-desktop-attachment/manifest.json",
    );

    // Restore the held candidate's exact wrong value: the schema's own `$id`.
    if let Some(object) = manifest.as_object_mut() {
        object.insert(
            "$schema".to_owned(),
            Value::String(
                "https://secode.local/fixtures/rust-reconstruction/oracle-receipt-manifest.schema.json"
                    .to_owned(),
            ),
        );
    }

    let mut errors = Vec::new();
    validate(&manifest, &schema, "", &mut errors);
    assert!(
        errors.iter().any(|error| error.contains("/$schema")),
        "the held candidate's $id form must be refused by the ratified const, got {errors:?}"
    );
}

/// Run the evaluator and return the message it panicked with, or `None` if it
/// completed without refusing.
///
/// The panic hook is deliberately NOT silenced. libtest captures a passing
/// test's output and discards it, so a caught panic prints nothing on a green
/// run. Installing a global no-op hook would instead swallow the diagnostics of
/// whichever other test in this binary happened to be panicking at the same
/// moment — and those diagnostics are exactly what a mutation battery reads to
/// tell one law's evidence from another's.
fn panic_message_from_validating(instance: &Value, schema: &Value) -> Option<String> {
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut errors = Vec::new();
        validate(instance, schema, "", &mut errors);
    }));
    match outcome {
        Ok(()) => None,
        Err(payload) => {
            if let Some(text) = payload.downcast_ref::<String>() {
                Some(text.clone())
            } else if let Some(text) = payload.downcast_ref::<&'static str>() {
                Some((*text).to_owned())
            } else {
                Some("<panic payload was not a string>".to_owned())
            }
        }
    }
}

#[test]
fn an_unimplemented_assertion_keyword_is_refused_rather_than_skipped() {
    // `enum` is a real assertion keyword this evaluator does not implement. It
    // cannot be reached through the ratified schema, which uses no unimplemented
    // keyword — a synthetic schema is the only way to drive the refusal arm at
    // all, which is why the guarantee went unwitnessed until now.
    let schema = json!({ "enum": ["retained", "rust"] });

    let Some(message) = panic_message_from_validating(&json!("retained"), &schema) else {
        panic!(
            "the evaluator SKIPPED the unimplemented assertion keyword `enum` and reported a \
             clean pass. Every keyword it cannot check must be refused, or this file reports \
             passes it never performed — the exact defect class the R1 review found in the \
             corpus owner."
        );
    };
    assert!(
        message.contains("enum") && message.contains("does not implement"),
        "the refusal must NAME the keyword it could not check, so a reader knows what went \
         unchecked; got {message:?}"
    );
}

#[test]
fn an_unknown_type_value_is_refused_rather_than_assumed() {
    // The second refusal arm, on the same reasoning: an unrecognised `type` name
    // that quietly passed would turn every constraint beneath it into an
    // unchecked one while still reporting success.
    let schema = json!({ "type": "endpoint" });

    let Some(message) = panic_message_from_validating(&json!("http://127.0.0.1:9993"), &schema)
    else {
        panic!(
            "the evaluator ACCEPTED the unknown `type` value \"endpoint\" instead of refusing \
             it. An unrecognised type name must never read as satisfied."
        );
    };
    assert!(
        message.contains("endpoint"),
        "the refusal must name the unrecognised type value; got {message:?}"
    );
}

#[test]
fn every_type_arm_separates_a_matching_instance_from_a_violating_one() {
    // Both directions for every arm. A table that only checked violations stays
    // green against an arm rewritten to `false`; one that only checked matches
    // stays green against an arm rewritten to `true` — and `true` is precisely
    // the mutant that survived the R2 review. Neither direction alone scores the
    // arm, so both are driven here.
    let cases: [(&str, Value, Value); 7] = [
        ("object", json!({}), json!([])),
        ("array", json!([]), json!({})),
        ("string", json!("retained"), json!(7)),
        ("integer", json!(7), json!(7.5)),
        ("number", json!(7.5), json!("7.5")),
        ("boolean", json!(true), json!("true")),
        ("null", Value::Null, json!(0)),
    ];

    for (name, matching, violating) in cases {
        let schema = json!({ "type": name });

        let mut accepted = Vec::new();
        validate(&matching, &schema, "", &mut accepted);
        assert!(
            accepted.is_empty(),
            "the `{name}` arm must ACCEPT the instance {matching}, but reported {accepted:?}"
        );

        let mut refused = Vec::new();
        validate(&violating, &schema, "", &mut refused);
        assert!(
            refused.len() == 1,
            "the `{name}` arm must report exactly one type violation for the instance \
             {violating}, but reported {refused:?}"
        );
        assert!(
            refused[0].contains("type must be"),
            "the `{name}` violation must say the type is wrong; got {:?}",
            refused[0]
        );
    }
}

#[test]
fn a_frozen_scenario_validates_under_the_same_evaluator() {
    // The independent control: a scenario this lane did not author, validated by
    // the same code against the same file. If this failed, a failure above would
    // be attributable to the evaluator rather than to the manifest.
    let schema = ratified_schema();
    // Chosen because the governed profile already stages this manifest
    // (`reasoning-manifest`), so the control travels to the Linux host without
    // adding a second artifact to reach it.
    let frozen = fixtures::fixture_path(Path::new(
        "provider-transcript-reasoning-text-usage-stop/manifest.json",
    ));
    let manifest = parse(&read(&frozen), "a frozen oracle-receipt scenario");

    let mut errors = Vec::new();
    validate(&manifest, &schema, "", &mut errors);
    assert!(
        errors.is_empty(),
        "a frozen scenario failed the evaluator, so the evaluator is wrong: {errors:?}"
    );
}

// ---------------------------------------------------------------------------
// R4D-2E — the operator preflight corpus, validated by the same evaluator
// against the same ratified file.
//
// Appended rather than folded into the block above so every byte of the
// reviewed R4D-1 and R4D-2A surface in this file is unchanged.
//
// It deliberately does NOT bring a second evaluator with it. A lane that
// validated its own manifest with its own parser would be the exact defect the
// R1 review found here: a corpus and an owner agreeing with each other while
// both diverge from the contract they name. The ratified schema file stays the
// single oracle, and this lane reuses the bounded evaluator that already panics
// on any keyword it does not implement.
// ---------------------------------------------------------------------------

fn operator_preflight_corpus_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/rust-reconstruction/r4-operator-preflight-contract")
}

#[test]
fn the_operator_preflight_manifest_validates_against_the_actual_ratified_schema() {
    let schema = ratified_schema();
    let manifest = parse(
        &read(&operator_preflight_corpus_dir().join("manifest.json")),
        "r4-operator-preflight-contract/manifest.json",
    );

    let mut errors = Vec::new();
    validate(&manifest, &schema, "", &mut errors);
    assert!(
        errors.is_empty(),
        "the operator preflight manifest does not validate against the ratified schema:\n  {}",
        errors.join("\n  ")
    );
}
