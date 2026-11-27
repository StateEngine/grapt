/// Every payload key any action record may carry. A kind's allowed payload is
/// this universe minus that kind's forbidden list, which is what makes the
/// vocabulary closed rather than merely enumerated.
const PAYLOAD_KEYS: [&str; 4] = ["plan", "expect", "answers", "target"];

#[derive(Debug, Clone, PartialEq, Eq)]
struct Scenario {
    id: String,
    evidence_path: String,
    test_target: String,
    test: String,
}

fn repo_root() -> PathBuf {
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/grapt-location -> crates -> repository root
    root.pop();
    root.pop();
    root
}

fn corpus_dir() -> PathBuf {
    repo_root().join("fixtures/rust-reconstruction/r4-host-auth-foundation")
}

/// Reads a required corpus file, refusing absence and unreadability alike.
fn read_required(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => panic!(
            "the corpus file {} must exist and be readable, but it could not be read: {error}",
            path.display()
        ),
    }
}

fn require_str(object: &Value, key: &str, context: &str) -> Result<String, String> {
    match object.get(key) {
        Some(Value::String(value)) if !value.is_empty() => Ok(value.clone()),
        Some(Value::String(_)) => Err(format!("{context}: `{key}` is empty")),
        Some(_) => Err(format!("{context}: `{key}` is not a string")),
        None => Err(format!("{context}: `{key}` is missing")),
    }
}

fn require_exact(object: &Value, key: &str, expected: &str, context: &str) -> Result<(), String> {
    let actual = require_str(object, key, context)?;
    if actual != expected {
        return Err(format!(
            "{context}: `{key}` declares {actual:?}, but this corpus is {expected:?}"
        ));
    }
    Ok(())
}

fn require_object<'a>(object: &'a Value, key: &str, context: &str) -> Result<&'a Value, String> {
    match object.get(key) {
        Some(value) if value.is_object() => Ok(value),
        Some(_) => Err(format!("{context}: `{key}` is not a JSON object")),
        None => Err(format!("{context}: `{key}` is missing")),
    }
}

/// Refuses any key outside `allowed`, and any key inside `forbidden`.
fn require_closed_keys(
    object: &Value,
    allowed: &[&str],
    forbidden: &[&str],
    context: &str,
) -> Result<(), String> {
    let Some(map) = object.as_object() else {
        return Err(format!("{context}: not a JSON object"));
    };
    for key in map.keys() {
        if forbidden.contains(&key.as_str()) {
            return Err(format!(
                "{context}: `{key}` is forbidden here and must not be declared"
            ));
        }
        if !allowed.contains(&key.as_str()) {
            return Err(format!(
                "{context}: `{key}` is not part of the declared contract; allowed keys are {allowed:?}"
            ));
        }
    }
    for key in allowed {
        if !map.contains_key(*key) {
            return Err(format!("{context}: `{key}` is missing"));
        }
    }
    Ok(())
}

fn require_nonempty_string_array(object: &Value, key: &str, context: &str) -> Result<(), String> {
    match object.get(key) {
        Some(Value::Array(rows)) if !rows.is_empty() => {
            for (index, row) in rows.iter().enumerate() {
                match row {
                    Value::String(value) if !value.is_empty() => {}
                    _ => {
                        return Err(format!(
                            "{context}: `{key}[{index}]` must be a nonempty string"
                        ))
                    }
                }
            }
            Ok(())
        }
        Some(Value::Array(_)) => Err(format!("{context}: `{key}` is empty")),
        Some(_) => Err(format!("{context}: `{key}` is not an array")),
        None => Err(format!("{context}: `{key}` is missing")),
    }
}

/// Validates the manifest as a closed `grapt-location-acceptance-corpus/v1`
/// document and returns its scenarios in declared order.
#[expect(clippy::too_many_lines, reason = "legacy")]
fn parse_manifest(src: &str) -> Result<Vec<Scenario>, String> {
    let root: Value = match serde_json::from_str(src) {
        Ok(value) => value,
        Err(error) => return Err(format!("manifest is not valid JSON: {error}")),
    };
    if !root.is_object() {
        return Err("manifest is not a JSON object".to_owned());
    }

    // The closed key set. `scenarios` is checked separately so the shared helper
    // can also serve the nested objects.
    let mut allowed: Vec<&str> = support::MANIFEST_KEYS.to_vec();
    allowed.push("scenarios");
    require_closed_keys(
        &root,
        &allowed,
        &support::MANIFEST_FORBIDDEN_KEYS,
        "manifest",
    )?;

    // The identity pins. These are exact values, not shapes: R1's first
    // counterexample renamed the contract to `fabricated-corpus-contract/v999`
    // and stayed green because only nonemptiness was required.
    require_exact(&root, "contract", support::CORPUS_CONTRACT, "manifest")?;
    require_exact(&root, "id", support::CORPUS_ID, "manifest")?;
    require_exact(&root, "lane", support::CORPUS_LANE, "manifest")?;
    require_exact(
        &root,
        "owning_crate",
        support::CORPUS_OWNING_CRATE,
        "manifest",
    )?;
    require_str(&root, "title", "manifest")?;
    require_str(&root, "contract_note", "manifest")?;
    require_str(&root, "law_pack_note", "manifest")?;

    // `schema_version` must be the exact number, not a string that looks like it.
    match root.get("schema_version") {
        Some(Value::Number(value)) if value.as_u64() == Some(support::CORPUS_SCHEMA_VERSION) => {}
        Some(Value::Number(value)) => {
            return Err(format!(
                "manifest: `schema_version` is {value}, but this corpus is version {}",
                support::CORPUS_SCHEMA_VERSION
            ))
        }
        Some(_) => return Err("manifest: `schema_version` is not a number".to_owned()),
        None => return Err("manifest: `schema_version` is missing".to_owned()),
    }

    let law = require_object(&root, "law_conformance", "manifest")?;
    require_closed_keys(
        law,
        &support::LAW_CONFORMANCE_KEYS,
        &["oracle_commit", "parity", "captured_at"],
        "manifest law_conformance",
    )?;
    for key in support::LAW_CONFORMANCE_KEYS {
        require_str(law, key, "manifest law_conformance")?;
    }

    let scope = require_object(&root, "scope", "manifest")?;
    require_closed_keys(scope, &support::SCOPE_KEYS, &[], "manifest scope")?;
    for key in support::SCOPE_KEYS {
        require_nonempty_string_array(scope, key, "manifest scope")?;
    }

    let determinism = require_object(&root, "determinism", "manifest")?;
    require_closed_keys(
        determinism,
        &support::DETERMINISM_KEYS,
        &[],
        "manifest determinism",
    )?;
    for key in support::DETERMINISM_KEYS {
        require_str(determinism, key, "manifest determinism")?;
    }

    let contracts = require_object(&root, "record_contracts", "manifest")?;
    require_closed_keys(contracts, &["actions"], &[], "manifest record_contracts")?;
    require_str(contracts, "actions", "manifest record_contracts")?;

    let paths = require_object(&root, "paths", "manifest")?;
    require_closed_keys(paths, &["actions"], &[], "manifest paths")?;
    require_exact(
        paths,
        "actions",
        support::CORPUS_ACTIONS_PATH,
        "manifest paths",
    )?;

    let rows = match root.get("scenarios") {
        Some(Value::Array(rows)) if !rows.is_empty() => rows,
        Some(Value::Array(_)) => return Err("manifest: `scenarios` is empty".to_owned()),
        Some(_) => return Err("manifest: `scenarios` is not an array".to_owned()),
        None => return Err("manifest: `scenarios` is missing".to_owned()),
    };

    let mut scenarios = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, row) in rows.iter().enumerate() {
        let context = format!("manifest scenario {index}");
        require_closed_keys(row, &support::SCENARIO_KEYS, &[], &context)?;
        let id = require_str(row, "id", &context)?;
        require_str(row, "title", &context)?;
        require_str(row, "claim", &context)?;
        if !seen.insert(id.clone()) {
            return Err(format!("duplicate scenario id {id:?}"));
        }
        let evidence = require_object(row, "evidence", &context)?;
        let evidence_context = format!("{context} evidence");
        require_closed_keys(evidence, &support::EVIDENCE_KEYS, &[], &evidence_context)?;
        scenarios.push(Scenario {
            id,
            evidence_path: require_str(evidence, "path", &evidence_context)?,
            test_target: require_str(evidence, "test_target", &evidence_context)?,
            test: require_str(evidence, "test", &evidence_context)?,
        });
    }
    Ok(scenarios)
}

/// Binds every scenario to the compiled integration-test target and the exact
/// owner the shared inventory names, and proves that owner is a test there.
///
/// `target_source` is injected so the negative tests can present a target that
/// does not contain the owner without touching the repository.
fn check_owners(scenarios: &[Scenario], target_source: &str) -> Result<(), String> {
    if scenarios.len() != support::OWNED_SCENARIO_COUNT {
        return Err(format!(
            "the corpus declares {} scenarios, but exactly {} are admitted",
            scenarios.len(),
            support::OWNED_SCENARIO_COUNT
        ));
    }

    let tests = support::test_function_names(target_source);
    for (scenario, admitted) in scenarios.iter().zip(support::OWNED_SCENARIOS.iter()) {
        if scenario.id != admitted.id {
            return Err(format!(
                "scenario {:?} is not the admitted scenario {:?}; the corpus must declare exactly the four admitted scenarios, in order",
                scenario.id, admitted.id
            ));
        }
        if scenario.test_target != support::CORPUS_TEST_TARGET {
            return Err(format!(
                "scenario {:?} declares test_target {:?}, but the only target that owns this corpus is {:?}",
                scenario.id, scenario.test_target, support::CORPUS_TEST_TARGET
            ));
        }
        if scenario.evidence_path != support::CORPUS_TEST_TARGET_PATH {
            return Err(format!(
                "scenario {:?} declares evidence path {:?}, but target {:?} is compiled from {:?}",
                scenario.id,
                scenario.evidence_path,
                support::CORPUS_TEST_TARGET,
                support::CORPUS_TEST_TARGET_PATH
            ));
        }
        if scenario.test != admitted.owner {
            return Err(format!(
                "scenario {:?} names owner {:?}, but its admitted executable owner is {:?}",
                scenario.id, scenario.test, admitted.owner
            ));
        }
        if !tests.iter().any(|name| name == &scenario.test) {
            return Err(format!(
                "scenario {:?} names owner {:?}, which is not a test in target {:?}; a helper or an absent function cannot own a scenario",
                scenario.id, scenario.test, scenario.test_target
            ));
        }
    }
    Ok(())
}

/// Validates one action record's payload against its kind.
fn check_action_payload(row: &Value, kind: &str, context: &str) -> Result<(), String> {
    let Some(contract) = support::action_kind(kind) else {
        return Err(format!(
            "{context}: kind {kind:?} is outside the declared record contract"
        ));
    };

    let mut allowed: Vec<&str> = support::ACTION_COMMON_KEYS.to_vec();
    for key in PAYLOAD_KEYS {
        if !contract.forbidden.contains(&key) {
            allowed.push(key);
        }
    }
    let Some(map) = row.as_object() else {
        return Err(format!("{context}: not a JSON object"));
    };
    for key in map.keys() {
        if contract.forbidden.contains(&key.as_str()) {
            return Err(format!("{context}: kind {kind:?} must not carry `{key}`"));
        }
        if !allowed.contains(&key.as_str()) {
            return Err(format!(
                "{context}: `{key}` is not part of the record contract for kind {kind:?}"
            ));
        }
    }
    for key in contract.required {
        if !map.contains_key(*key) {
            return Err(format!("{context}: kind {kind:?} requires `{key}`"));
        }
    }

    match kind {
        "listener.bind" => {
            let plan = require_object(row, "plan", context)?;
            require_closed_keys(plan, &support::PLAN_KEYS, &[], &format!("{context} plan"))?;
            if let Some(expect) = row.get("expect") {
                match expect.get("acquired") {
                    Some(Value::Bool(_)) => {}
                    Some(_) => {
                        return Err(format!("{context}: `expect.acquired` is not a boolean"))
                    }
                    None => {
                        return Err(format!(
                            "{context}: a bind that states an expectation must state `acquired`"
                        ))
                    }
                }
            }
        }
        "resolver.configure" => match row.get("answers") {
            Some(Value::Array(rows)) if !rows.is_empty() => {
                for (index, answer) in rows.iter().enumerate() {
                    let answer_context = format!("{context} answers[{index}]");
                    require_str(answer, "name", &answer_context)?;
                    require_nonempty_string_array(answer, "addresses", &answer_context)?;
                }
            }
            _ => return Err(format!("{context}: `answers` must be a nonempty array")),
        },
        "listener.close" => {
            let expect = require_object(row, "expect", context)?;
            if expect.get("released") != Some(&Value::Bool(true)) {
                return Err(format!(
                    "{context}: a close record must expect `released: true`"
                ));
            }
        }
        "listener.connect" => {
            require_str(row, "target", context)?;
            let expect = require_object(row, "expect", context)?;
            if expect.get("connection_completed") != Some(&Value::Bool(true)) {
                return Err(format!(
                    "{context}: a connect record must expect `connection_completed: true`"
                ));
            }
        }
        "expect.refusal" => {
            let expect = require_object(row, "expect", context)?;
            require_str(expect, "classification", &format!("{context} expect"))?;
        }
        "listener.observe-receipt" => {
            // Shape only here. An observation cannot be typed against one record
            // in isolation, because what it is allowed to claim depends on the
            // bind plan of its own scenario. `check_nested_semantics` is the
            // authority and every observation reaches it, since a record naming
            // an undeclared scenario is already refused above.
            require_object(row, "expect", context)?;
        }
        _ => unreachable!("kind was resolved against the closed vocabulary above"),
    }
    Ok(())
}

/// Validates the action log: closed vocabulary, kind-specific payloads, and the
/// outcome each scenario's claim requires.
fn check_actions(src: &str, scenarios: &[Scenario]) -> Result<(), String> {
    let known: BTreeSet<&str> = scenarios.iter().map(|row| row.id.as_str()).collect();
    let mut covered = BTreeSet::new();
    let mut previous_ordinal: Option<i64> = None;
    // Per scenario: the last bind's stated `acquired`, and any refusal seen.
    let mut terminal_bind: Vec<(String, Option<bool>)> = Vec::new();
    let mut refusals: Vec<(String, String)> = Vec::new();
    // Every record, kept so the nested payloads can be typed against the other
    // records in their own scenario rather than in isolation.
    let mut collected: Vec<(String, String, Value)> = Vec::new();

    for (index, line) in src.lines().enumerate() {
        let number = index + 1;
        if line.trim().is_empty() {
            return Err(format!("actions line {number} is blank"));
        }
        let row: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(error) => return Err(format!("actions line {number} is not valid JSON: {error}")),
        };
        if !row.is_object() {
            return Err(format!("actions line {number} is not a JSON object"));
        }

        let ordinal = match row.get("ordinal") {
            Some(Value::Number(value)) => match value.as_i64() {
                Some(ordinal) => ordinal,
                None => {
                    return Err(format!(
                        "actions line {number}: `ordinal` is not an integer"
                    ))
                }
            },
            _ => {
                return Err(format!(
                    "actions line {number}: `ordinal` is missing or not a number"
                ))
            }
        };
        if let Some(previous) = previous_ordinal {
            if ordinal <= previous {
                return Err(format!(
                    "actions line {number}: ordinal {ordinal} does not follow {previous}; ordinals must be unique and strictly increasing"
                ));
            }
        }
        previous_ordinal = Some(ordinal);

        let context = format!("actions line {number}");
        let scenario = require_str(&row, "scenario", &context)?;
        if !known.contains(scenario.as_str()) {
            return Err(format!(
                "{context}: scenario {scenario:?} is not declared in the manifest"
            ));
        }
        covered.insert(scenario.clone());

        require_str(&row, "actor", &context)?;
        let kind = require_str(&row, "kind", &context)?;
        check_action_payload(&row, &kind, &context)?;

        if kind == "listener.bind" {
            let acquired = row
                .get("expect")
                .and_then(|expect| expect.get("acquired"))
                .and_then(Value::as_bool);
            terminal_bind.retain(|(id, _)| id != &scenario);
            terminal_bind.push((scenario.clone(), acquired));
        }
        if kind == "expect.refusal" {
            let classification = row
                .get("expect")
                .and_then(|expect| expect.get("classification"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            refusals.push((scenario.clone(), classification));
        }
        collected.push((scenario, kind, row));
    }

    if previous_ordinal.is_none() {
        return Err("actions log is empty".to_owned());
    }
    for scenario in scenarios {
        if !covered.contains(&scenario.id) {
            return Err(format!(
                "scenario {:?} is declared in the manifest but carries no action records",
                scenario.id
            ));
        }
    }

    check_outcomes(&terminal_bind, &refusals)?;
    check_nested_semantics(&collected)
}

/// The semantic half: what the log states must be what the scenario claims.
fn check_outcomes(
    terminal_bind: &[(String, Option<bool>)],
    refusals: &[(String, String)],
) -> Result<(), String> {
    for admitted in support::OWNED_SCENARIOS.iter() {
        let Some((_, acquired)) = terminal_bind.iter().find(|(id, _)| id == admitted.id) else {
            return Err(format!(
                "scenario {:?} carries no `listener.bind` record, so it states no bind outcome at all",
                admitted.id
            ));
        };
        let Some(acquired) = acquired else {
            return Err(format!(
                "scenario {:?}: its last `listener.bind` states no `expect.acquired`, so the record does not say whether the bind was meant to succeed",
                admitted.id
            ));
        };
        if *acquired != admitted.terminal_bind_acquired {
            return Err(format!(
                "scenario {:?}: its last `listener.bind` expects acquired={acquired}, but the scenario claim requires acquired={}",
                admitted.id, admitted.terminal_bind_acquired
            ));
        }

        let seen: Vec<&str> = refusals
            .iter()
            .filter(|(id, _)| id == admitted.id)
            .map(|(_, classification)| classification.as_str())
            .collect();
        match admitted.refusal_classification {
            Some(expected) => {
                if seen.len() != 1 {
                    return Err(format!(
                        "scenario {:?} is a refusal scenario and must carry exactly one `expect.refusal`, found {}",
                        admitted.id,
                        seen.len()
                    ));
                }
                if seen[0] != expected {
                    return Err(format!(
                        "scenario {:?} refuses with classification {:?}, but its claim requires {expected:?}",
                        admitted.id, seen[0]
                    ));
                }
            }
            None => {
                if !seen.is_empty() {
                    return Err(format!(
                        "scenario {:?} claims a successful bind and must carry no `expect.refusal`, found {seen:?}",
                        admitted.id
                    ));
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Nested payload semantics.
//
// The R1 closure review found a survivor here: a fixed-bind observation could
// claim `requested_host {"literal":"203.0.113.9"}`, `requested_port "ephemeral"`
// and `observed_address "203.0.113.9:0"` while its scenario claimed exact fixed
// loopback, because `listener.observe-receipt` was required only to be a
// nonempty object. Nested values are now typed by shape, by value, and against
// the other records of the same scenario.
// ---------------------------------------------------------------------------
