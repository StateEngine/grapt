//! The session-list acceptance corpus gate.
//!
//! Two laws, kept apart on purpose:
//!
//! 1. the manifest validates against the RATIFIED manifest schema — the actual
//!    `fixtures/rust-reconstruction/manifest.schema.json`, read from the frozen
//!    fixture root, not a second copy of its rules living here; and
//! 2. every action in `actions.jsonl` is EXECUTED against this candidate's
//!    store and session-list implementation, and the API, event, durable and
//!    projection surfaces it really produces are compared with the declared
//!    expectations.
//!
//! Deserialising the corpus and asserting a few of its own values proves
//! nothing about the implementation: the fixture would agree with itself while
//! the code under it was arbitrarily wrong. Missing or structurally invalid
//! evidence fails loudly here rather than being skipped.

use grapt_session::{
    BoundedSearch, NonNegativeMs, ParentFilter, SessionListCriteria, SessionListDirection,
    SessionListError, SessionListKey, SessionListLimits, SessionListOrder, SessionListProjector,
    SessionListScope, SessionListStore,
};
use grapt_store::{AggregateId, AppendInput, EventStore, MigrationOutcome, Projector};
use grapt_types::{EventVersion, LocationRef, SessionId, WorkspaceId};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[path = "../../../test-support/fixtures.rs"]
mod fixtures;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Fallible<T> = Result<T, Box<dyn std::error::Error>>;

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/rust-reconstruction/session-list-keyset-v1")
}

fn read_json(path: &Path) -> Fallible<Value> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("{} is unreadable: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("{} is not valid JSON: {error}", path.display()).into())
}

fn read_jsonl(path: &Path) -> Fallible<Vec<Value>> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("{} is unreadable: {error}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line)
                .map_err(|error| format!("{} line {}: {error}", path.display(), index + 1).into())
        })
        .collect()
}

// ---------------------------------------------------------------------------
// JSON Schema validation, over exactly the keyword set the ratified schema uses
// ---------------------------------------------------------------------------

/// Keywords that carry no assertion and may be ignored safely.
const ANNOTATIONS: [&str; 4] = ["$schema", "$id", "title", "description"];

/// Validates `instance` against `schema`.
///
/// Any keyword this validator does not implement is a hard error rather than a
/// silent pass. That is the whole safety property: a validator that quietly
/// ignores an unknown keyword under-validates, and a fixture could then be
/// mutated straight through the gap it left. If the ratified schema ever grows
/// a keyword, this gate goes red and says which one instead of getting weaker.
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn validate(schema: &Value, instance: &Value, at: &str, errors: &mut Vec<String>) {
    let Some(object) = schema.as_object() else {
        if schema == &Value::Bool(false) {
            errors.push(format!("{at}: schema `false` admits nothing"));
        }
        return;
    };

    for (keyword, argument) in object {
        if ANNOTATIONS.contains(&keyword.as_str()) {
            continue;
        }
        match keyword.as_str() {
            "type" => {
                let expected = argument.as_str().unwrap_or("<non-string type>");
                let matches = match expected {
                    "object" => instance.is_object(),
                    "array" => instance.is_array(),
                    "string" => instance.is_string(),
                    "integer" => instance.is_i64() || instance.is_u64(),
                    "number" => instance.is_number(),
                    "boolean" => instance.is_boolean(),
                    "null" => instance.is_null(),
                    other => {
                        errors.push(format!("{at}: unimplemented type `{other}`"));
                        true
                    }
                };
                if !matches {
                    errors.push(format!("{at}: expected type {expected}"));
                }
            }
            "const" => {
                if instance != argument {
                    errors.push(format!("{at}: expected const {argument}, found {instance}"));
                }
            }
            "pattern" => {
                if let Some(text) = instance.as_str() {
                    let source = argument.as_str().unwrap_or_default();
                    match regex_lite::Regex::new(source) {
                        Ok(regex) => {
                            if !regex.is_match(text) {
                                errors.push(format!("{at}: {text:?} does not match /{source}/"));
                            }
                        }
                        Err(error) => {
                            errors.push(format!(
                                "{at}: schema pattern /{source}/ is invalid: {error}"
                            ));
                        }
                    }
                }
            }
            "minLength" => {
                if let (Some(text), Some(minimum)) = (instance.as_str(), argument.as_u64()) {
                    if (text.chars().count() as u64) < minimum {
                        errors.push(format!("{at}: shorter than minLength {minimum}"));
                    }
                }
            }
            "required" => {
                if let (Some(fields), Some(map)) = (argument.as_array(), instance.as_object()) {
                    for field in fields {
                        if let Some(name) = field.as_str() {
                            if !map.contains_key(name) {
                                errors.push(format!("{at}: missing required property `{name}`"));
                            }
                        }
                    }
                }
            }
            "properties" => {
                if let (Some(schemas), Some(map)) = (argument.as_object(), instance.as_object()) {
                    for (name, subschema) in schemas {
                        if let Some(value) = map.get(name) {
                            validate(subschema, value, &format!("{at}/{name}"), errors);
                        }
                    }
                }
            }
            "additionalProperties" => {
                if argument != &Value::Bool(false) {
                    errors.push(format!(
                        "{at}: unimplemented additionalProperties form {argument}"
                    ));
                } else if let Some(map) = instance.as_object() {
                    let declared = object
                        .get("properties")
                        .and_then(Value::as_object)
                        .map(|properties| properties.keys().cloned().collect::<Vec<_>>())
                        .unwrap_or_default();
                    for name in map.keys() {
                        if !declared.contains(name) {
                            errors.push(format!("{at}: property `{name}` is not permitted"));
                        }
                    }
                }
            }
            "minItems" | "maxItems" => {
                if let (Some(items), Some(bound)) = (instance.as_array(), argument.as_u64()) {
                    let length = items.len() as u64;
                    let violated = if keyword == "minItems" {
                        length < bound
                    } else {
                        length > bound
                    };
                    if violated {
                        errors.push(format!("{at}: {length} items violates {keyword} {bound}"));
                    }
                }
            }
            "prefixItems" => {
                if let (Some(schemas), Some(items)) = (argument.as_array(), instance.as_array()) {
                    for (index, subschema) in schemas.iter().enumerate() {
                        if let Some(value) = items.get(index) {
                            validate(subschema, value, &format!("{at}/{index}"), errors);
                        }
                    }
                }
            }
            "items" => {
                let prefix = object
                    .get("prefixItems")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                if let Some(items) = instance.as_array() {
                    if argument == &Value::Bool(false) {
                        if items.len() > prefix {
                            errors
                                .push(format!("{at}: more than {prefix} items are not permitted"));
                        }
                    } else if argument.is_object() {
                        // Applies to every element after any prefixItems.
                        for (index, value) in items.iter().enumerate().skip(prefix) {
                            validate(argument, value, &format!("{at}/{index}"), errors);
                        }
                    } else {
                        errors.push(format!("{at}: unimplemented items form {argument}"));
                    }
                }
            }
            "oneOf" => {
                if let Some(branches) = argument.as_array() {
                    let satisfied = branches
                        .iter()
                        .filter(|branch| {
                            let mut ignored = Vec::new();
                            validate(branch, instance, at, &mut ignored);
                            ignored.is_empty()
                        })
                        .count();
                    if satisfied != 1 {
                        errors.push(format!(
                            "{at}: exactly one oneOf branch must match, {satisfied} did"
                        ));
                    }
                }
            }
            "not" => {
                let mut ignored = Vec::new();
                validate(argument, instance, at, &mut ignored);
                if ignored.is_empty() {
                    errors.push(format!("{at}: the `not` subschema must not match"));
                }
            }
            other => errors.push(format!(
                "{at}: unimplemented schema keyword `{other}` — this validator refuses to \
                 pass an instance it cannot fully check"
            )),
        }
    }
}

#[test]
fn the_corpus_manifest_validates_against_the_ratified_schema() -> TestResult {
    let root = corpus_root();
    let manifest = read_json(&root.join("manifest.json"))?;

    // The schema is the ratified artefact, resolved through the same frozen
    // fixture root the rest of this workspace's fixtures use. `fixture_path`
    // asserts presence, so an absent ratified root fails loudly instead of
    // quietly skipping the only structural gate this corpus has.
    let declared = manifest["$schema"]
        .as_str()
        .ok_or("the manifest must declare its schema")?;
    assert_eq!(
        declared, "../manifest.schema.json",
        "the corpus must declare one of the two ratified manifest contracts"
    );
    let schema_path = fixtures::fixture_path(Path::new("manifest.schema.json"));
    let schema = read_json(&schema_path)?;

    let mut errors = Vec::new();
    validate(&schema, &manifest, "manifest", &mut errors);
    assert!(
        errors.is_empty(),
        "the manifest does not satisfy {}:\n  {}",
        schema_path.display(),
        errors.join("\n  ")
    );

    // A schema-valid manifest may still name paths that do not exist.
    assert_eq!(manifest["id"], "session-list-keyset-v1");
    let mut declared_paths = vec![
        manifest["paths"]["initial_state"].clone(),
        manifest["paths"]["actions"].clone(),
    ];
    let expected = manifest["paths"]["expected"]
        .as_object()
        .ok_or("expected paths must be an object")?;
    declared_paths.extend(expected.values().cloned());
    for entry in declared_paths {
        let relative = entry
            .as_str()
            .ok_or("every declared path must be a string")?;
        let path = root.join(relative);
        assert!(path.is_file(), "declared path {relative} does not exist");
    }

    // The law-pack pin must be the CURRENT one, not merely well-shaped. The
    // schema can only check that nine ordered paths carry 64 hex characters;
    // comparing against the ratified `law-pack.json` is what makes the pin
    // exact.
    let ratified = read_json(&fixtures::fixture_path(Path::new("law-pack.json")))?;
    assert_eq!(
        manifest["law_pack"]["revision"], ratified["revision"],
        "law-pack revision drifted"
    );
    assert_eq!(
        manifest["law_pack"]["sources"], ratified["sources"],
        "the nine ordered law sources or their digests drifted from the ratified pin"
    );
    assert_eq!(
        manifest["law_pack"]["matrix"], ratified["matrix"],
        "the supersession matrix pin drifted"
    );
    Ok(())
}

include!("session_list_acceptance_corpus/execution.rs");
include!("session_list_acceptance_corpus/comparison.rs");
