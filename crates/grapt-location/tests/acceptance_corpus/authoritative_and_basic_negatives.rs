#[test]
fn the_corpus_is_structurally_valid_and_every_scenario_is_executably_owned() {
    let dir = corpus_dir();
    let manifest = read_required(&dir.join("manifest.json"));
    let actions = read_required(&dir.join("actions.jsonl"));

    let scenarios = match parse_manifest(&manifest) {
        Ok(scenarios) => scenarios,
        Err(error) => panic!("the corpus manifest is invalid: {error}"),
    };

    let ids: Vec<&str> = scenarios.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(
        ids,
        support::scenario_ids(),
        "the corpus must contain exactly the four admitted L scenarios, in order"
    );

    if let Err(error) = check_owners(&scenarios, TARGET_SOURCE) {
        panic!("the corpus does not bind its scenarios to executable owners: {error}");
    }
    if let Err(error) = check_actions(&actions, &scenarios) {
        panic!("the corpus action log is invalid: {error}");
    }
}

// ---------------------------------------------------------------------------
// Negatives: every guard is shown to fail.
// ---------------------------------------------------------------------------

#[test]
fn the_control_fixtures_are_accepted() {
    // Without this, every negative below could be passing for the wrong reason.
    let scenarios = parse_or_panic(&manifest_fixture());
    assert!(
        check_owners(&scenarios, TARGET_SOURCE).is_ok(),
        "the control manifest must bind cleanly"
    );
    assert!(
        check_actions(&actions_fixture(), &scenarios).is_ok(),
        "the control action log must be accepted"
    );
}

#[test]
fn a_fabricated_contract_or_version_is_refused() {
    let base = manifest_fixture();

    let wrong_contract = corrupt(
        &base,
        support::CORPUS_CONTRACT,
        "fabricated-corpus-contract/v999",
    );
    match parse_manifest(&wrong_contract) {
        Err(error) => assert!(
            error.contains("contract"),
            "the refusal must name the contract, got {error}"
        ),
        Ok(_) => panic!("a fabricated contract value must be refused"),
    }

    let wrong_version = corrupt(&base, r#""schema_version":1"#, r#""schema_version":999"#);
    assert!(
        parse_manifest(&wrong_version).is_err(),
        "a fabricated schema_version must be refused"
    );

    let version_as_string = corrupt(&base, r#""schema_version":1"#, r#""schema_version":"1""#);
    assert!(
        parse_manifest(&version_as_string).is_err(),
        "a schema_version that is a string rather than a number must be refused"
    );
}

#[test]
fn an_unknown_or_forbidden_manifest_key_is_refused() {
    let base = manifest_fixture();

    let unknown = corrupt(&base, r#""title":"t""#, r#""title":"t","invented":"x""#);
    assert!(
        parse_manifest(&unknown).is_err(),
        "an unknown top-level key must be refused; the contract is closed"
    );

    // Ruling 2 forbids an unverifiable law_pack block in this tree.
    let law_pack = corrupt(
        &base,
        r#""title":"t""#,
        r#""title":"t","law_pack":{"sha256":"deadbeef"}"#,
    );
    match parse_manifest(&law_pack) {
        Err(error) => assert!(
            error.contains("law_pack"),
            "the refusal must name law_pack, got {error}"
        ),
        Ok(_) => panic!("a law_pack block must be refused"),
    }

    // The corpus may not claim oracle capture or parity.
    let oracle = corrupt(
        &base,
        r#""basis":"b""#,
        r#""basis":"b","oracle_commit":"42e4888cc0""#,
    );
    assert!(
        parse_manifest(&oracle).is_err(),
        "an oracle_commit claim must be refused"
    );
}

#[test]
fn a_malformed_manifest_is_refused() {
    let base = manifest_fixture();
    let cases = [
        ("truncated JSON", "{\"contract\":".to_owned()),
        ("not an object", "[]".to_owned()),
        ("missing contract", r#"{"id":"i"}"#.to_owned()),
        (
            "declared action log renamed",
            corrupt(
                &base,
                r#""actions":"actions.jsonl""#,
                r#""actions":"elsewhere.jsonl""#,
            ),
        ),
        (
            "scenario missing its evidence",
            corrupt(&base, r#","evidence""#, r#","not_evidence""#),
        ),
        (
            "determinism surface dropped",
            corrupt(&base, r#""filesystem":"f""#, r#""fs":"f""#),
        ),
        (
            "scope surface emptied",
            corrupt(&base, r#""preserves":["p"]"#, r#""preserves":[]"#),
        ),
    ];
    for (label, src) in cases {
        assert!(
            parse_manifest(&src).is_err(),
            "a manifest with {label} must be refused"
        );
    }
}

#[test]
fn a_duplicate_scenario_id_is_refused() {
    let base = manifest_fixture();
    let src = corrupt(
        &base,
        support::OWNED_SCENARIOS[1].id,
        support::OWNED_SCENARIOS[0].id,
    );
    match parse_manifest(&src) {
        Err(error) => assert!(
            error.contains("duplicate scenario id"),
            "the refusal must name the duplicate, got {error}"
        ),
        Ok(_) => panic!("a duplicate scenario id must be refused"),
    }
}

#[test]
fn a_fabricated_owner_or_target_is_refused() {
    let base = manifest_fixture();

    // R1's second counterexample: a real file, a nonexistent target, and an
    // ordinary helper rather than a test.
    let fabricated_target = corrupt(
        &base,
        &format!(r#""test_target":"{}""#, support::CORPUS_TEST_TARGET),
        r#""test_target":"fabricated_target_that_does_not_exist""#,
    );
    match check_owners(&parse_or_panic(&fabricated_target), TARGET_SOURCE) {
        Err(error) => assert!(
            error.contains("test_target"),
            "the refusal must name the target, got {error}"
        ),
        Ok(()) => {
            panic!("a scenario naming a target that does not own this corpus must be refused")
        }
    }

    let fabricated_owner = corrupt(&base, support::OWNED_SCENARIOS[0].owner, "repo_root");
    match check_owners(&parse_or_panic(&fabricated_owner), TARGET_SOURCE) {
        Err(error) => assert!(
            error.contains("executable owner"),
            "the refusal must name the admitted owner, got {error}"
        ),
        Ok(()) => panic!("a scenario owned by a helper must be refused"),
    }

    let elsewhere = corrupt(
        &base,
        support::CORPUS_TEST_TARGET_PATH,
        "crates/grapt-location/tests/acceptance_corpus.rs",
    );
    assert!(
        check_owners(&parse_or_panic(&elsewhere), TARGET_SOURCE).is_err(),
        "a scenario naming an evidence file that is not the declared target must be refused"
    );
}

#[test]
fn an_owner_absent_from_the_compiled_target_is_refused() {
    // The manifest and the inventory agree, but the target no longer defines
    // the test. This is the rename case that the compile-time array in
    // `listener_contract` turns into a build failure; here it is a refusal.
    let scenarios = parse_or_panic(&manifest_fixture());
    let hollow_target = "#[tokio::test]\nasync fn something_else() {}\n";
    match check_owners(&scenarios, hollow_target) {
        Err(error) => assert!(
            error.contains("not a test in target"),
            "the refusal must say the owner is not a test there, got {error}"
        ),
        Ok(()) => panic!("an owner absent from the target must be refused"),
    }

    // A helper of the right name is still not a test.
    let helper_only = format!("fn {}() {{}}\n", support::OWNED_SCENARIOS[0].owner);
    assert!(
        check_owners(&scenarios, &helper_only).is_err(),
        "a function without a test attribute must not own a scenario"
    );
}

#[test]
fn the_target_parser_reads_attributes_not_substrings() {
    let source = "#[tokio::test]\nasync fn bind_twice() {}\nfn bind() {}\n";
    let names = support::test_function_names(source);
    assert_eq!(
        names,
        vec!["bind_twice".to_owned()],
        "only the attributed function is a test; the bare `fn bind` is a helper"
    );
}

#[test]
fn a_malformed_action_log_is_refused() {
    let scenarios = parse_or_panic(&manifest_fixture());
    let good = actions_fixture();
    let Some(first) = good.lines().next().map(str::to_owned) else {
        panic!("the control action fixture must carry at least one record");
    };

    let cases = [
        ("empty log", String::new()),
        ("not JSON", "not json".to_owned()),
        ("blank line inside the log", format!("{good}\n\n{first}")),
        ("repeated ordinal", format!("{good}\n{first}")),
        (
            "undeclared scenario",
            corrupt(&good, support::OWNED_SCENARIOS[0].id, "invented"),
        ),
        (
            "kind outside the record contract",
            corrupt(
                &good,
                r#""kind":"listener.bind""#,
                r#""kind":"http.request""#,
            ),
        ),
    ];
    for (label, src) in cases {
        assert!(
            check_actions(&src, &scenarios).is_err(),
            "an action log with {label} must be refused"
        );
    }
}

#[test]
fn an_action_payload_that_disagrees_with_its_kind_is_refused() {
    let scenarios = parse_or_panic(&manifest_fixture());
    let good = actions_fixture();

    let no_plan = corrupt(
        &good,
        r#","plan":{"host":{"literal":"127.0.0.1"},"port":"ephemeral","resolution_bound":4,"attempt_bound":4}"#,
        "",
    );
    match check_actions(&no_plan, &scenarios) {
        Err(error) => assert!(
            error.contains("requires `plan`"),
            "the refusal must name the missing plan, got {error}"
        ),
        Ok(()) => panic!("a bind with no plan must be refused"),
    }

    let foreign_payload = corrupt(
        &good,
        r#""kind":"listener.bind","plan""#,
        r#""kind":"listener.bind","answers":[],"plan""#,
    );
    assert!(
        check_actions(&foreign_payload, &scenarios).is_err(),
        "a bind carrying resolver answers must be refused"
    );

    let short_plan = corrupt(&good, r#""attempt_bound":4"#, r#""attempts":4"#);
    assert!(
        check_actions(&short_plan, &scenarios).is_err(),
        "a plan missing a declared field must be refused"
    );

    let unknown_key = corrupt(
        &good,
        r#""actor":"harness""#,
        r#""actor":"harness","invented":1"#,
    );
    assert!(
        check_actions(&unknown_key, &scenarios).is_err(),
        "an unknown record key must be refused; the record contract is closed"
    );
}

#[test]
fn an_outcome_that_contradicts_the_scenario_claim_is_refused() {
    let scenarios = parse_or_panic(&manifest_fixture());
    let good = actions_fixture();

    // R1's third counterexample, exactly: the fixed-bind scenario's terminal
    // bind claims failure while its claim requires a successful bind.
    // The first record in the control log is scenario 0's terminal bind, and
    // `corrupt` replaces only the first occurrence, so this flips exactly it.
    let flipped = corrupt(
        &good,
        r#""expect":{"acquired":true}"#,
        r#""expect":{"acquired":false}"#,
    );
    match check_actions(&flipped, &scenarios) {
        Err(error) => assert!(
            error.contains("claim requires acquired=true"),
            "the refusal must name the contradiction, got {error}"
        ),
        Ok(()) => panic!("a terminal bind contradicting its scenario claim must be refused"),
    }

    // A refusal scenario that states the wrong classification.
    let wrong_classification = corrupt(&good, "address-in-use", "something-else");
    assert!(
        check_actions(&wrong_classification, &scenarios).is_err(),
        "a refusal with the wrong classification must be refused"
    );

    // A success scenario that smuggles in a refusal record.
    let stray_refusal = format!(
        "{good}\n{{\"ordinal\":900,\"scenario\":\"{}\",\"actor\":\"harness\",\"kind\":\"expect.refusal\",\"expect\":{{\"classification\":\"address-in-use\"}}}}",
        support::OWNED_SCENARIOS[0].id
    );
    assert!(
        check_actions(&stray_refusal, &scenarios).is_err(),
        "a successful scenario carrying a refusal must be refused"
    );
}

#[test]
fn the_r1_closure_survivor_is_refused() {
    // The exact mutant the closure review preserved: a fixed-bind observation
    // claiming a different host, an ephemeral requested port, and an observed
    // address on that foreign host at port zero. Every one of those three is a
    // separate refusal, so no single fix can mask the others.
    let scenarios = parse_or_panic(&manifest_fixture());
    let good = actions_fixture();

    let survivor = corrupt(
        &good,
        &format!(r#""requested_host":{{"literal":"{}"}}"#, support::LOOPBACK),
        r#""requested_host":{"literal":"203.0.113.9"}"#,
    );
    match check_actions(&survivor, &scenarios) {
        Err(error) => assert!(
            error.contains("terminal bind planned"),
            "the refusal must relate the observation to its bind plan, got {error}"
        ),
        Ok(()) => panic!("an observation naming a host its scenario never bound must be refused"),
    }

    let wrong_port = corrupt(
        &good,
        &format!(r#""requested_port":{{"fixed":"{FIXTURE_PORT}"}}"#),
        r#""requested_port":"ephemeral""#,
    );
    match check_actions(&wrong_port, &scenarios) {
        Err(error) => assert!(
            error.contains("terminal bind planned"),
            "the refusal must relate the requested port to its bind plan, got {error}"
        ),
        Ok(()) => panic!("an observation contradicting its plan's port intent must be refused"),
    }

    let zero_port = corrupt(
        &good,
        &format!(
            r#""observed_address":"{}:{FIXTURE_PORT}""#,
            support::LOOPBACK
        ),
        &format!(r#""observed_address":"{}:0""#, support::LOOPBACK),
    );
    match check_actions(&zero_port, &scenarios) {
        Err(error) => assert!(
            error.contains("ephemeral request, never an acquired port"),
            "the refusal must reject the zero port, got {error}"
        ),
        Ok(()) => panic!("an observed port of zero must be refused"),
    }

    let foreign_address = corrupt(
        &good,
        &format!(
            r#""observed_address":"{}:{FIXTURE_PORT}""#,
            support::LOOPBACK
        ),
        &format!(r#""observed_address":"203.0.113.9:{FIXTURE_PORT}""#),
    );
    assert!(
        check_actions(&foreign_address, &scenarios).is_err(),
        "an observed address on a host the scenario never bound must be refused"
    );
}
