//! The usage-compatibility acceptance corpus gate.
//!
//! Three laws, kept apart on purpose:
//!
//! 1. the manifest validates against the RATIFIED schema — the actual
//!    `fixtures/rust-reconstruction/oracle-receipt-manifest.schema.json`, read
//!    from the frozen fixture root, not a second copy of its rules living
//!    here;
//! 2. every action in `actions.jsonl` is EXECUTED against the real
//!    constructors in [`grapt_types::money`] and [`grapt_types::usage_compat`],
//!    and the outcome the code really produces is compared with the recorded
//!    expectation; and
//! 3. the aggregate in `expected/state.json` is RECOMPUTED from that run, so a
//!    stale expectation cannot quietly agree with a stale action.
//!
//! Deserialising the corpus and asserting a few of its own values would prove
//! nothing: the fixture would agree with itself while the code under it was
//! arbitrarily wrong. Missing or structurally invalid evidence fails loudly
//! here rather than being skipped into a pass.
//!
//! The schema validator below is bounded and REFUSES on any keyword it does
//! not implement. A validator that silently ignores an unknown keyword is
//! worse than no validator, because the next constraint added to the ratified
//! schema would be mutated straight through the gap it left.

use grapt_types::money::{MoneyError, MoneyUsd};
use grapt_types::usage::UsageField;
use grapt_types::usage_compat::{
    TokenCount, TokenCountError, TokenUsageInfo, UsageCompatError, MAX_EXACT_TOKEN_COUNT,
};
use regex_lite::Regex;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[path = "../../../test-support/fixtures.rs"]
mod fixtures;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Fallible<T> = Result<T, Box<dyn std::error::Error>>;

/// This lane's corpus lives in the rust tree beside the crate, while the
/// ratified SCHEMA lives in the frozen fixture root. The two are resolved
/// differently on purpose and neither substitutes for the other.
fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/rust-reconstruction/usage-compat-vocabulary-v1")
}

fn read_json(path: &Path) -> Fallible<Value> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    Ok(serde_json::from_str(&text)?)
}

fn read_actions() -> Fallible<Vec<Value>> {
    let path = corpus_dir().join("actions.jsonl");
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut rows = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .map_err(|error| format!("{}:{}: {error}", path.display(), index + 1))?;
        rows.push(value);
    }
    if rows.is_empty() {
        return Err(format!(
            "{} carries no actions; an empty corpus proves nothing",
            path.display()
        )
        .into());
    }
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Bounded JSON Schema validation, over exactly the keyword set the ratified
// schema uses. Anything else is an explicit refusal, never a silent skip.
// ---------------------------------------------------------------------------

/// Keywords that carry no assertion and are safe to pass over.
const ANNOTATIONS: [&str; 4] = ["$schema", "$id", "title", "description"];

/// One way of breaking the manifest, used by the gate's own negative cases.
type ManifestMutation<'a> = &'a dyn Fn(&mut Map<String, Value>);

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn validate(schema: &Value, instance: &Value, at: &str, errors: &mut Vec<String>) {
    if schema == &Value::Bool(false) {
        errors.push(format!("{at}: schema `false` admits nothing"));
        return;
    }
    if schema == &Value::Bool(true) {
        return;
    }
    let Some(object) = schema.as_object() else {
        errors.push(format!("{at}: schema is neither an object nor a boolean"));
        return;
    };
    for (keyword, argument) in object {
        if ANNOTATIONS.contains(&keyword.as_str()) {
            continue;
        }
        match keyword.as_str() {
            "type" => {
                let expected = argument.as_str().unwrap_or_default();
                let actual = match instance {
                    Value::Null => "null",
                    Value::Bool(_) => "boolean",
                    Value::Number(_) => "number",
                    Value::String(_) => "string",
                    Value::Array(_) => "array",
                    Value::Object(_) => "object",
                };
                let ok =
                    expected == actual || (expected == "integer" && instance.as_i64().is_some());
                if !ok {
                    errors.push(format!("{at}: expected type {expected}, found {actual}"));
                }
            }
            "const" => {
                if instance != argument {
                    errors.push(format!("{at}: expected const {argument}, found {instance}"));
                }
            }
            "required" => {
                if let (Some(names), Some(map)) = (argument.as_array(), instance.as_object()) {
                    for name in names {
                        let name = name.as_str().unwrap_or_default();
                        if !map.contains_key(name) {
                            errors.push(format!("{at}: required property `{name}` is missing"));
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
                if argument == &Value::Bool(false) {
                    if let Some(map) = instance.as_object() {
                        let declared = object
                            .get("properties")
                            .and_then(Value::as_object)
                            .map(|properties| properties.keys().cloned().collect::<Vec<_>>())
                            .unwrap_or_default();
                        for name in map.keys() {
                            if !declared.contains(name) {
                                errors.push(format!("{at}: `{name}` is not a declared property"));
                            }
                        }
                    }
                } else {
                    errors.push(format!(
                        "{at}: only `additionalProperties: false` is implemented"
                    ));
                }
            }
            "minLength" => {
                if let (Some(minimum), Some(text)) = (argument.as_u64(), instance.as_str()) {
                    if (text.chars().count() as u64) < minimum {
                        errors.push(format!("{at}: shorter than minLength {minimum}"));
                    }
                }
            }
            "pattern" => {
                let source = argument.as_str().unwrap_or_default();
                match Regex::new(source) {
                    Ok(regex) => {
                        if let Some(text) = instance.as_str() {
                            if !regex.is_match(text) {
                                errors.push(format!("{at}: `{text}` does not match /{source}/"));
                            }
                        }
                    }
                    Err(error) => {
                        errors.push(format!("{at}: pattern /{source}/ is invalid: {error}"));
                    }
                }
            }
            "minItems" => {
                if let (Some(minimum), Some(items)) = (argument.as_u64(), instance.as_array()) {
                    if (items.len() as u64) < minimum {
                        errors.push(format!("{at}: fewer than minItems {minimum}"));
                    }
                }
            }
            "maxItems" => {
                if let (Some(maximum), Some(items)) = (argument.as_u64(), instance.as_array()) {
                    if (items.len() as u64) > maximum {
                        errors.push(format!("{at}: more than maxItems {maximum}"));
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
                if let Some(items) = instance.as_array() {
                    let offset = object
                        .get("prefixItems")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0);
                    for (index, value) in items.iter().enumerate().skip(offset) {
                        validate(argument, value, &format!("{at}/{index}"), errors);
                    }
                }
            }
            "not" => {
                let mut inner = Vec::new();
                validate(argument, instance, at, &mut inner);
                if inner.is_empty() {
                    errors.push(format!("{at}: the `not` subschema must not match"));
                }
            }
            "oneOf" => {
                if let Some(branches) = argument.as_array() {
                    let matched = branches
                        .iter()
                        .filter(|branch| {
                            let mut inner = Vec::new();
                            validate(branch, instance, at, &mut inner);
                            inner.is_empty()
                        })
                        .count();
                    if matched != 1 {
                        errors.push(format!(
                            "{at}: oneOf matched {matched} branches, expected exactly 1"
                        ));
                    }
                }
            }
            other => errors.push(format!(
                "{at}: unimplemented schema keyword `{other}` — this validator refuses \
                 rather than silently admitting a constraint it cannot check"
            )),
        }
    }
}

#[test]
fn the_manifest_validates_against_the_ratified_schema_file() -> TestResult {
    let manifest = read_json(&corpus_dir().join("manifest.json"))?;

    let declared = manifest["$schema"]
        .as_str()
        .ok_or("the manifest must declare its schema")?;
    assert_eq!(
        declared, "../oracle-receipt-manifest.schema.json",
        "this corpus is oracle-backed and must declare the oracle-receipt schema"
    );

    // The RATIFIED artefact, resolved through the frozen fixture root the rest
    // of the workspace uses. `fixture_path` panics loudly when it is absent,
    // which is the honest outcome: a schema check with no schema is not a
    // check.
    let schema_path = fixtures::fixture_path(Path::new("oracle-receipt-manifest.schema.json"));
    let schema = read_json(&schema_path)?;

    let mut errors = Vec::new();
    validate(&schema, &manifest, "manifest", &mut errors);
    assert!(
        errors.is_empty(),
        "manifest does not satisfy {}:\n  {}",
        schema_path.display(),
        errors.join("\n  ")
    );

    assert_eq!(manifest["id"], "usage-compat-vocabulary-v1");
    assert_eq!(
        manifest["oracle"]["commit"], "430859db2dd049b3b2767c083af4f14f83379911",
        "the corpus must pin the exact frozen commit its shapes were read from"
    );

    // Schema validity does not make a declared path exist.
    for relative in [
        "actions.jsonl",
        "expected/durable.jsonl",
        "expected/state.json",
        "expected/waivers.json",
    ] {
        let path = corpus_dir().join(relative);
        assert!(
            path.exists(),
            "manifest declares {relative} but {} is absent",
            path.display()
        );
    }
    Ok(())
}

#[test]
fn the_schema_validator_refuses_the_manifests_it_must_refuse() -> TestResult {
    // A gate that never fires is decoration. Each case breaks the manifest a
    // different way; if any is accepted, a passing real check proves nothing.
    let schema_path = fixtures::fixture_path(Path::new("oracle-receipt-manifest.schema.json"));
    let schema = read_json(&schema_path)?;
    let good = read_json(&corpus_dir().join("manifest.json"))?;

    let mut baseline = Vec::new();
    validate(&schema, &good, "manifest", &mut baseline);
    assert!(
        baseline.is_empty(),
        "the control must pass or the refusals below prove nothing: {baseline:?}"
    );

    let mutate = |mutation: ManifestMutation<'_>| -> Vec<String> {
        let mut broken = good.clone();
        if let Some(map) = broken.as_object_mut() {
            mutation(map);
        }
        let mut errors = Vec::new();
        validate(&schema, &broken, "manifest", &mut errors);
        errors
    };

    // A compile-time-sized table: adding a refusal case without extending the
    // length, or dropping one, fails to compile rather than quietly shrinking
    // the set of routes this gate is proven to close.
    let cases: [(&str, ManifestMutation<'_>); 7] = [
        ("a missing required key", &|map| {
            map.remove("evidence");
        }),
        ("an undeclared extra key", &|map| {
            map.insert("smuggled".into(), json!(1));
        }),
        ("a wrong const on $schema", &|map| {
            map.insert("$schema".into(), json!("../manifest.schema.json"));
        }),
        ("a wrong const on schema_version", &|map| {
            map.insert("schema_version".into(), json!(2));
        }),
        ("an id that breaks its pattern", &|map| {
            map.insert("id".into(), json!("Usage_Compat_V1"));
        }),
        ("a commit that breaks its pattern", &|map| {
            map.insert(
                "oracle".into(),
                json!({"branch": "grapt", "commit": "not-a-sha"}),
            );
        }),
        (
            "both oracle and law_conformance, which oneOf forbids",
            &|map| {
                map.insert(
                    "law_conformance".into(),
                    json!({
                        "basis": "ratified-law-pack",
                        "oracle_status": "not-implemented-in-frozen-typescript-engine",
                        "note": "n/a"
                    }),
                );
            },
        ),
        // An empty evidence array would defeat minItems.
    ];
    for (label, mutation) in cases {
        assert!(
            !mutate(mutation).is_empty(),
            "the validator admitted a manifest carrying {label}"
        );
    }

    // The refuse-on-unknown-keyword property itself, proven rather than
    // asserted in a comment.
    let mut errors = Vec::new();
    validate(&json!({"multipleOf": 3}), &json!(4), "probe", &mut errors);
    assert!(
        errors.iter().any(|error| error.contains("unimplemented")),
        "an unimplemented keyword must refuse, not be skipped: {errors:?}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Executing the corpus against the real vocabulary.
// ---------------------------------------------------------------------------

fn money_error_label(error: MoneyError) -> (&'static str, &'static str) {
    match error {
        MoneyError::NotFinite(kind) => ("not-finite", kind.label()),
        MoneyError::SumNotFinite(kind) => ("sum-not-finite", kind.label()),
    }
}

fn token_error_label(error: TokenCountError) -> (&'static str, Option<&'static str>) {
    match error {
        TokenCountError::NotFinite(kind) => ("not-finite", Some(kind.label())),
        TokenCountError::Negative => ("negative", None),
        TokenCountError::NotIntegral => ("not-integral", None),
        TokenCountError::AboveExactBound { .. } => ("above-exact-bound", None),
        TokenCountError::ContractNumberAboveExactBound { .. } => ("above-exact-bound", None),
        TokenCountError::SumAboveExactBound => ("sum-above-exact-bound", None),
    }
}

fn refused_money(error: MoneyError) -> Map<String, Value> {
    let (label, detail) = money_error_label(error);
    let mut observed = Map::new();
    observed.insert("outcome".into(), json!("refused"));
    observed.insert("error".into(), json!(label));
    observed.insert("detail".into(), json!(detail));
    observed
}

fn accepted_money(value: MoneyUsd) -> Map<String, Value> {
    let mut observed = Map::new();
    observed.insert("outcome".into(), json!("accepted"));
    observed.insert("amount".into(), json!(value.amount()));
    observed.insert(
        "bitwise_positive_zero".into(),
        json!(value.amount().to_bits() == 0.0f64.to_bits()),
    );
    observed
}

fn refused_token(error: TokenCountError) -> Map<String, Value> {
    let (label, detail) = token_error_label(error);
    let mut observed = Map::new();
    observed.insert("outcome".into(), json!("refused"));
    observed.insert("error".into(), json!(label));
    if let TokenCountError::ContractNumberAboveExactBound { value_bits } = error {
        observed.insert("value_bits".into(), json!(format!("0x{value_bits:016x}")));
    }
    if let Some(detail) = detail {
        observed.insert("detail".into(), json!(detail));
    }
    observed
}

fn accepted_token(value: TokenCount) -> Map<String, Value> {
    let mut observed = Map::new();
    observed.insert("outcome".into(), json!("accepted"));
    observed.insert("value".into(), json!(value.value().to_string()));
    observed
}

fn refused_usage(error: UsageCompatError) -> Map<String, Value> {
    let mut observed = Map::new();
    observed.insert("outcome".into(), json!("refused"));
    match error {
        UsageCompatError::FieldAbsent(field) => {
            observed.insert("error".into(), json!("field-absent"));
            observed.insert("field".into(), json!(field.label()));
        }
        UsageCompatError::FieldNotCarriable { field, error } => {
            let (cause, _) = token_error_label(error);
            observed.insert("error".into(), json!("field-not-carriable"));
            observed.insert("field".into(), json!(field.label()));
            observed.insert("cause".into(), json!(cause));
        }
    }
    observed
}

fn accepted_usage(usage: TokenUsageInfo) -> Map<String, Value> {
    let mut observed = Map::new();
    observed.insert("outcome".into(), json!("accepted"));
    observed.insert(
        "labelled_counts".into(),
        Value::Array(
            usage
                .labelled_counts()
                .iter()
                .map(|(label, count)| json!([label, count.to_string()]))
                .collect(),
        ),
    );
    observed
}

/// A monetary amount from the corpus. The three values JSON cannot spell are
/// carried as strings, and so is the `f64::MAX` sentinel.
fn money_input(action: &Value, key: &str) -> Fallible<f64> {
    match &action[key] {
        Value::Number(_) => action[key]
            .as_f64()
            .ok_or_else(|| format!("{key} is not representable as f64").into()),
        Value::String(text) => match text.as_str() {
            "nan" => Ok(f64::NAN),
            "+inf" => Ok(f64::INFINITY),
            "-inf" => Ok(f64::NEG_INFINITY),
            "f64::MAX" => Ok(f64::MAX),
            other => Err(format!("unknown monetary sentinel `{other}`").into()),
        },
        other => Err(format!("{key} must be a number or a sentinel string, found {other}").into()),
    }
}

/// An unsigned token count from the corpus. ALWAYS a decimal string: this
/// preserves the exact integer independently of the fixture reader. Contract
/// numbers still enter through `money_input`, because that public edge receives
/// an actual binary64 and its above-bound refusal pins the exact supplied bits.
fn count_input(action: &Value, key: &str) -> Fallible<u64> {
    let text = action[key]
        .as_str()
        .ok_or_else(|| format!("{key} must be a decimal STRING, not a JSON number"))?;
    Ok(text.parse::<u64>()?)
}

fn signed_input(action: &Value, key: &str) -> Fallible<i64> {
    let text = action[key]
        .as_str()
        .ok_or_else(|| format!("{key} must be a decimal STRING, not a JSON number"))?;
    Ok(text.parse::<i64>()?)
}

/// An optional observation: `null` in the corpus is ABSENT, which is the whole
/// point of the field-absent rows and is never spelled as 0.
fn observed_input(action: &Value, key: &str) -> Fallible<UsageField> {
    match &action[key] {
        Value::Null => Ok(UsageField::ABSENT),
        Value::String(text) => Ok(UsageField::reported(TokenCount::new(text.parse::<u64>()?)?)),
        other => Err(format!("{key} must be a decimal string or null, found {other}").into()),
    }
}

fn execute(action: &Value) -> Fallible<Option<Map<String, Value>>> {
    let kind = action["action"]
        .as_str()
        .ok_or("every action must name its action")?;
    let observed = match kind {
        "note" => return Ok(None),
        "money-from-contract-number" => match MoneyUsd::new(money_input(action, "amount")?) {
            Ok(value) => accepted_money(value),
            Err(error) => refused_money(error),
        },
        "money-from-legacy-column" => {
            match MoneyUsd::from_legacy_column(money_input(action, "amount")?) {
                Ok(value) => accepted_money(value),
                Err(error) => refused_money(error),
            }
        }
        "money-checked-add" => {
            let left = MoneyUsd::new(money_input(action, "left")?)?;
            let right = MoneyUsd::new(money_input(action, "right")?)?;
            match left.checked_add(right) {
                Ok(value) => accepted_money(value),
                Err(error) => refused_money(error),
            }
        }
        "token-count-new" => match TokenCount::new(count_input(action, "value")?) {
            Ok(value) => accepted_token(value),
            Err(error) => refused_token(error),
        },
        "token-count-from-legacy-column" => {
            match TokenCount::from_legacy_column(signed_input(action, "value")?) {
                Ok(value) => accepted_token(value),
                Err(error) => refused_token(error),
            }
        }
        "token-count-from-contract-number" => {
            match TokenCount::from_contract_number(money_input(action, "value")?) {
                Ok(value) => accepted_token(value),
                Err(error) => refused_token(error),
            }
        }
        "token-count-checked-add" => {
            let left = TokenCount::new(count_input(action, "left")?)?;
            let right = TokenCount::new(count_input(action, "right")?)?;
            match left.checked_add(right) {
                Ok(value) => accepted_token(value),
                Err(error) => refused_token(error),
            }
        }
        "usage-from-observed" => {
            let projected = TokenUsageInfo::from_observed(
                observed_input(action, "input")?,
                observed_input(action, "output")?,
                observed_input(action, "reasoning")?,
                observed_input(action, "cache_read")?,
                observed_input(action, "cache_write")?,
            );
            match projected {
                Ok(usage) => accepted_usage(usage),
                Err(error) => refused_usage(error),
            }
        }
        "usage-from-legacy-columns" => {
            let projected = TokenUsageInfo::from_legacy_columns(
                signed_input(action, "input")?,
                signed_input(action, "output")?,
                signed_input(action, "reasoning")?,
                signed_input(action, "cache_read")?,
                signed_input(action, "cache_write")?,
            );
            match projected {
                Ok(usage) => accepted_usage(usage),
                Err(error) => refused_usage(error),
            }
        }
        "usage-field-order" => {
            let mut observed = Map::new();
            observed.insert("outcome".into(), json!("accepted"));
            observed.insert(
                "field_labels".into(),
                json!(TokenUsageInfo::FIELD_LABELS.to_vec()),
            );
            observed.insert("field_count".into(), json!(TokenUsageInfo::FIELD_COUNT));
            observed
        }
        other => return Err(format!("unknown corpus action `{other}`").into()),
    };
    Ok(Some(observed))
}

#[test]
fn every_action_is_executed_against_the_vocabulary() -> TestResult {
    let actions = read_actions()?;
    let mut executed = 0usize;
    for action in &actions {
        let ordinal = action["ordinal"]
            .as_u64()
            .ok_or("every action needs an ordinal")?;
        let scenario = action["scenario"].as_str().unwrap_or("<unnamed>");
        let Some(observed) = execute(action)? else {
            continue;
        };
        executed += 1;
        let expect = action["expect"]
            .as_object()
            .ok_or_else(|| format!("action {ordinal} carries no expectation"))?;
        for (key, expected) in expect {
            let actual = observed.get(key).ok_or_else(|| {
                format!(
                    "action {ordinal} ({scenario}) expects `{key}`, which this run did not observe"
                )
            })?;
            // JSON does not distinguish 0 from 0.0, so a monetary amount is
            // compared as a NUMBER rather than as a JSON token. The comparison
            // is still exact f64 equality — nothing is rounded or tolerated.
            let agrees = match (actual.as_f64(), expected.as_f64()) {
                (Some(left), Some(right)) => left == right,
                _ => actual == expected,
            };
            assert!(
                agrees,
                "action {ordinal} ({scenario}): `{key}` — the vocabulary produced {actual}, \
                 the corpus records {expected}"
            );
        }
    }
    assert!(
        executed >= 30,
        "only {executed} actions executed; a corpus this small would not cover the leaves"
    );
    Ok(())
}

#[test]
fn the_aggregate_state_is_recomputed_from_the_run() -> TestResult {
    let actions = read_actions()?;
    let mut accepted = 0u64;
    let mut refused = 0u64;
    let mut notes = 0u64;
    let mut census: BTreeMap<String, u64> = BTreeMap::new();

    for action in &actions {
        let Some(observed) = execute(action)? else {
            notes += 1;
            continue;
        };
        match observed.get("outcome").and_then(Value::as_str) {
            Some("accepted") => accepted += 1,
            Some("refused") => {
                refused += 1;
                let error = observed
                    .get("error")
                    .and_then(Value::as_str)
                    .ok_or("a refusal must name its error")?;
                *census.entry(error.to_owned()).or_default() += 1;
            }
            other => return Err(format!("unknown outcome {other:?}").into()),
        }
    }

    let state = read_json(&corpus_dir().join("expected/state.json"))?;
    assert_eq!(state["totals"]["actions"], json!(actions.len()));
    assert_eq!(state["totals"]["notes"], json!(notes));
    assert_eq!(
        state["totals"]["accepted"],
        json!(accepted),
        "accepted total is recomputed from the run, not copied from the actions file"
    );
    assert_eq!(state["totals"]["refused"], json!(refused));

    let recorded = state["refusals_by_error"]
        .as_object()
        .ok_or("state.json must carry a refusal census")?;
    let derived: Map<String, Value> = census
        .iter()
        .map(|(error, count)| (error.clone(), json!(count)))
        .collect();
    assert_eq!(
        recorded, &derived,
        "the refusal census must match the errors the vocabulary really produced"
    );

    // The leaf law is pinned in the fixture as well as in the type, so a
    // reordering has to be made twice to pass.
    assert_eq!(
        state["field_labels"],
        json!(TokenUsageInfo::FIELD_LABELS.to_vec())
    );
    assert_eq!(state["field_count"], json!(TokenUsageInfo::FIELD_COUNT));
    assert_eq!(
        state["max_exact_token_count"],
        json!(MAX_EXACT_TOKEN_COUNT.to_string())
    );
    Ok(())
}

#[test]
fn the_durable_surface_is_empty_and_that_is_asserted() -> TestResult {
    // Empty BY LAW: grapt-types declares no library dependencies, so it has
    // no store, codec or socket to write a durable row through. Reading the
    // file and proving it empty is the difference between a law and a file
    // nobody opened.
    let path = corpus_dir().join("expected/durable.jsonl");
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    assert!(
        text.trim().is_empty(),
        "{} carries durable rows, but this lane owns a value vocabulary in the root \
         crate and has no durable surface to produce them; a row here means that \
         property was broken",
        path.display()
    );
    Ok(())
}

#[test]
fn every_waiver_is_dispositioned_rather_than_merely_listed() -> TestResult {
    let waivers = read_json(&corpus_dir().join("expected/waivers.json"))?;
    let rows = waivers["waivers"]
        .as_array()
        .ok_or("waivers.json must carry a waivers array")?;
    assert!(
        !rows.is_empty(),
        "this lane has disclosures and must record them"
    );
    for row in rows {
        for key in ["id", "subject", "statement", "disposition", "evidence"] {
            let value = row[key]
                .as_str()
                .ok_or_else(|| format!("waiver {} lacks {key}", row["id"]))?;
            assert!(
                !value.trim().is_empty(),
                "waiver {} has an empty {key}; an undisclosed waiver is worse than none",
                row["id"]
            );
        }
    }
    Ok(())
}

#[test]
fn the_corpus_carries_the_synthetic_cache_write_row_it_needs() -> TestResult {
    // cache_write is zero on all 305 rows of the evidence corpus, so a
    // correctly-read cache.write and an invented zero are indistinguishable by
    // value against real data alone. If this row ever disappears the leaf goes
    // unwitnessed while every other test still passes.
    let actions = read_actions()?;
    let witnessed = actions.iter().any(|action| {
        action["action"] == "usage-from-legacy-columns"
            && action["cache_write"]
                .as_str()
                .is_some_and(|text| text != "0")
            && action["expect"]["outcome"] == "accepted"
    });
    assert!(
        witnessed,
        "no accepted legacy row carries a NON-ZERO cache.write; without it the leaf is \
         unwitnessable from this corpus (R4D-2G preflight blocker B-G4)"
    );
    Ok(())
}
