#[test]
fn a_placeholder_port_control_log_is_accepted() {
    // Without this, every placeholder refusal below could be passing because the
    // placeholder path is broken outright rather than because its law fired.
    let scenarios = parse_or_panic(&manifest_fixture());
    let control = placeholder_control();
    assert!(
        check_actions(&control.log, &scenarios).is_ok(),
        "a log whose ports are written as placeholders naming real producing ordinals must be accepted"
    );
}

#[test]
fn the_r2_closure_survivor_is_refused() {
    // The exact mutant the closure review preserved: the fixed-bind
    // observation's observed_address port replaced by an arbitrary angle-bracket
    // token. Shape alone used to be enough, so this passed 16/16 twice.
    let scenarios = parse_or_panic(&manifest_fixture());
    let control = placeholder_control();

    let survivor = corrupt(
        &control.log,
        &format!(
            r#""observed_address":"{}:{}""#,
            support::LOOPBACK,
            control.terminal
        ),
        &format!(
            r#""observed_address":"{}:<fabricated non-port token>""#,
            support::LOOPBACK
        ),
    );
    match check_actions(&survivor, &scenarios) {
        Err(error) => assert!(
            error.contains("is not a port; an observed port is written exactly"),
            "an arbitrary angle-bracket token must be refused as malformed, got {error}"
        ),
        Ok(()) => panic!("an arbitrary `<...>` token must not be accepted as an observed port"),
    }
}

#[test]
fn a_placeholder_naming_the_wrong_source_is_refused() {
    // Two refusals that must not mask one another. Naming an ordinal that
    // observes no port is a different fault from naming a real producer that is
    // not the one the terminal bind carries — and the second is only reachable
    // because the control scenario borrows two ports.
    let scenarios = parse_or_panic(&manifest_fixture());
    let control = placeholder_control();
    let address = format!(
        r#""observed_address":"{}:{}""#,
        support::LOOPBACK,
        control.terminal
    );

    let no_such_producer = corrupt(
        &control.log,
        &address,
        &format!(
            r#""observed_address":"{}:{PLACEHOLDER_PREFIX}99>""#,
            support::LOOPBACK
        ),
    );
    match check_actions(&no_such_producer, &scenarios) {
        Err(error) => assert!(
            error.contains("observes no port in this scenario"),
            "naming an ordinal that observes nothing must be refused for that reason, got {error}"
        ),
        Ok(()) => panic!("a placeholder naming a record that observes no port must be refused"),
    }

    let wrong_producer = corrupt(
        &control.log,
        &address,
        &format!(
            r#""observed_address":"{}:{}""#,
            support::LOOPBACK,
            control.other
        ),
    );
    match check_actions(&wrong_producer, &scenarios) {
        Err(error) => assert!(
            error.contains("is not the port this scenario's terminal fixed bind carries"),
            "naming a real but different producer must be refused for that reason, got {error}"
        ),
        Ok(()) => panic!(
            "an observation naming a port other than the one its terminal bind carries must be refused"
        ),
    }
}

#[test]
fn a_port_placeholder_is_judged_by_grammar_source_order_and_terminal_identity() {
    // The four refusals of `check_observed_port_token`, exercised directly so
    // each branch has its own witness. The precedence branch has no corpus-level
    // witness: every admitted scenario's producer precedes every record that
    // names it, so a forward reference cannot be built from the admitted shapes.
    let producers = [2_u64, 5];
    let terminal = format!("{PLACEHOLDER_PREFIX}5>");
    let law = PortLaw {
        producers: &producers,
        at: 7,
        terminal: Some(PortIdentity::Symbolic { ordinal: 5 }),
    };

    assert!(
        check_observed_port_token(&terminal, &law, "ctx").is_ok(),
        "the terminal bind's own producing ordinal must be accepted"
    );

    let forward = PortLaw {
        producers: &producers,
        at: 3,
        terminal: None,
    };

    // `expect_err` is denied workspace-wide alongside `unwrap`, so each outcome
    // is matched rather than unwrapped.
    let cases = [
        (
            "a placeholder with no ordinal is not a port",
            check_observed_port_token("<port observed at ordinal>", &law, "ctx"),
            "is not a port; an observed port is written exactly",
        ),
        (
            "a leading zero would give one ordinal two spellings",
            check_observed_port_token(&format!("{PLACEHOLDER_PREFIX}05>"), &law, "ctx"),
            "is not a port; an observed port is written exactly",
        ),
        (
            "ordinal 3 observes no port here",
            check_observed_port_token(&format!("{PLACEHOLDER_PREFIX}3>"), &law, "ctx"),
            "observes no port in this scenario",
        ),
        (
            "a port cannot be observed after it is used",
            check_observed_port_token(&format!("{PLACEHOLDER_PREFIX}5>"), &forward, "ctx"),
            "does not precede this record",
        ),
        (
            "ordinal 2 is a real producer, but not the terminal bind's",
            check_observed_port_token(&format!("{PLACEHOLDER_PREFIX}2>"), &law, "ctx"),
            "is not the port this scenario's terminal fixed bind carries",
        ),
    ];
    for (why, outcome, expected) in cases {
        match outcome {
            Err(error) => assert!(
                error.contains(expected),
                "{why}: the refusal must say {expected:?}, got {error}"
            ),
            Ok(()) => panic!("{why}"),
        }
    }
}

#[test]
fn a_port_is_related_as_a_typed_identity_not_as_a_spelling() {
    // The R3 closure survivor: the relation used to live inside the placeholder
    // branch, so a literal was range-checked and accepted whatever the terminal
    // carried. Both identity kinds are now compared in one place, and the four
    // combinations are asserted here — two ACCEPT, two REFUSE — so no single
    // branch can carry the law for another.
    let producers = [1_u64];
    let symbolic = PortLaw {
        producers: &producers,
        at: 4,
        terminal: Some(PortIdentity::Symbolic { ordinal: 1 }),
    };
    let concrete = PortLaw {
        producers: &producers,
        at: 4,
        terminal: Some(PortIdentity::Concrete { port: 40001 }),
    };

    // Accepting cases: an identity equal to the terminal's, in either kind.
    for (why, outcome) in [
        (
            "a symbolic terminal accepts the same symbolic identity",
            check_observed_port_token(&format!("{PLACEHOLDER_PREFIX}1>"), &symbolic, "ctx"),
        ),
        (
            "a concrete terminal accepts the same concrete port",
            check_observed_port_token("40001", &concrete, "ctx"),
        ),
    ] {
        assert!(outcome.is_ok(), "{why}: got {outcome:?}");
    }

    // Refusing cases, each with its own message so none can mask another.
    let cases = [
        (
            "a literal is not the observed port merely by being a port",
            check_observed_port_token("40001", &symbolic, "ctx"),
            "cannot be related to",
        ),
        (
            "a concrete terminal refuses a different concrete port",
            check_observed_port_token("40002", &concrete, "ctx"),
            "but this scenario's terminal fixed bind carries port 40001",
        ),
        (
            "a symbolic identity is never coerced to a concrete one",
            check_observed_port_token(&format!("{PLACEHOLDER_PREFIX}1>"), &concrete, "ctx"),
            "is never coerced to a concrete one",
        ),
    ];
    for (why, outcome, expected) in cases {
        match outcome {
            Err(error) => assert!(
                error.contains(expected),
                "{why}: the refusal must say {expected:?}, got {error}"
            ),
            Ok(()) => panic!("{why}"),
        }
    }

    // An ephemeral-terminal scenario establishes no relation, and inventing one
    // would be an invented standard rather than this lane's law.
    let unrelated = PortLaw {
        producers: &producers,
        at: 4,
        terminal: None,
    };
    assert!(
        check_observed_port_token("40001", &unrelated, "ctx").is_ok(),
        "a scenario with no terminal fixed bind relates its ports to nothing"
    );
}

#[test]
fn a_literal_port_is_refused_at_every_related_position() {
    // P2 / P2b / P2c: a plain decimal walked through observed_address, a
    // refusal address and a connect target alike. Each position is asserted
    // separately here, against the placeholder control, so one position being
    // fixed cannot mask another still being open.
    let scenarios = parse_or_panic(&manifest_fixture());
    let control = placeholder_control();
    let literal = "40001";

    let positions = [
        ("observed_address", &control.observed_address),
        ("a refusal address", &control.refusal_address),
        ("a connect target", &control.connect_target),
    ];
    for (position, fragment) in positions {
        let mutated = corrupt(
            &control.log,
            fragment,
            &with_literal_port(fragment, literal),
        );
        match check_actions(&mutated, &scenarios) {
            Err(error) => assert!(
                error.contains("cannot be related to"),
                "{position}: a literal port must be refused for its own reason, got {error}"
            ),
            Ok(()) => panic!("{position}: a literal port must not survive here"),
        }
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn a_hostile_bind_plan_or_nested_value_is_refused() {
    let scenarios = parse_or_panic(&manifest_fixture());
    let good = actions_fixture();

    let cases = [
        (
            "a plan host the scenario never binds",
            corrupt(
                &good,
                &format!(r#""host":{{"literal":"{}"}}"#, support::LOOPBACK),
                r#""host":{"literal":"203.0.113.9"}"#,
            ),
        ),
        (
            "a plan host carrying both shapes at once",
            corrupt(
                &good,
                &format!(r#""host":{{"literal":"{}"}}"#, support::LOOPBACK),
                &format!(
                    r#""host":{{"literal":"{}","name":"wide.test"}}"#,
                    support::LOOPBACK
                ),
            ),
        ),
        (
            "a plan host resolved by name where a literal is admitted",
            corrupt(
                &good,
                &format!(r#""host":{{"literal":"{}"}}"#, support::LOOPBACK),
                r#""host":{"name":"wide.test"}"#,
            ),
        ),
        (
            "a fixed port of zero",
            corrupt(
                &good,
                &format!(r#""port":{{"fixed":"{FIXTURE_PORT}"}}"#),
                r#""port":{"fixed":0}"#,
            ),
        ),
        (
            "an invented bare port intent",
            corrupt(&good, r#""port":"ephemeral""#, r#""port":"whatever""#),
        ),
        (
            "a resolution bound of zero",
            corrupt(&good, r#""resolution_bound":4"#, r#""resolution_bound":0"#),
        ),
        (
            "an observation claiming a zero observed port",
            corrupt(
                &good,
                r#""observed_port_is_zero":false"#,
                r#""observed_port_is_zero":true"#,
            ),
        ),
        (
            "an ephemeral request that asks for a port other than zero",
            corrupt(
                &good,
                r#""requested_port_asks_for":0"#,
                r#""requested_port_asks_for":8080"#,
            ),
        ),
        (
            "a refusal classified from message text",
            corrupt(
                &good,
                r#""classified_from_message_text":false"#,
                r#""classified_from_message_text":true"#,
            ),
        ),
        (
            "a refusal classified from something other than the error kind",
            corrupt(
                &good,
                &format!(r#""classified_from":"{}""#, support::CLASSIFIED_FROM),
                r#""classified_from":"message-text""#,
            ),
        ),
        (
            "a refusal naming an address on a foreign host",
            corrupt(
                &good,
                &format!(r#""address":"{}:{FIXTURE_PORT}""#, support::LOOPBACK),
                &format!(r#""address":"203.0.113.9:{FIXTURE_PORT}""#),
            ),
        ),
        (
            "a produced count that does not exceed its limit",
            corrupt(&good, r#""produced":3"#, r#""produced":2"#),
        ),
        (
            "a before-bind refusal that admits an attempted bind",
            corrupt(&good, r#""bind_attempts":0"#, r#""bind_attempts":1"#),
        ),
        (
            "a resolver answering a name this scenario never resolves",
            corrupt(
                &good,
                &format!(r#""name":"{}""#, support::RESOLVED_NAME),
                r#""name":"elsewhere.test""#,
            ),
        ),
        (
            "an observation key outside the scenario's pinned set",
            corrupt(
                &good,
                r#""observed_port_is_zero":false"#,
                r#""observed_port_is_zero":false,"invented":1"#,
            ),
        ),
        (
            "an observation attached to a scenario that declares none",
            format!(
                "{good}\n{{\"ordinal\":900,\"scenario\":\"{}\",\"actor\":\"harness\",\"kind\":\"listener.observe-receipt\",\"expect\":{{\"observed_host\":\"{}\"}}}}",
                support::OWNED_SCENARIOS[2].id,
                support::LOOPBACK
            ),
        ),
        (
            "a connect targeting a host the scenario never bound",
            format!(
                "{good}\n{{\"ordinal\":901,\"scenario\":\"{}\",\"actor\":\"harness\",\"kind\":\"listener.connect\",\"target\":\"203.0.113.9:{FIXTURE_PORT}\",\"expect\":{{\"connection_completed\":true}}}}",
                support::OWNED_SCENARIOS[2].id
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
fn a_scenario_with_no_actions_is_refused() {
    let scenarios = parse_or_panic(&manifest_fixture());
    let partial = actions_fixture();
    let kept: Vec<&str> = partial
        .lines()
        .filter(|line| !line.contains(support::OWNED_SCENARIOS[3].id))
        .collect();
    match check_actions(&kept.join("\n"), &scenarios) {
        Err(error) => assert!(
            error.contains("carries no action records"),
            "the refusal must name the uncovered scenario, got {error}"
        ),
        Ok(()) => panic!("a declared scenario with no action records must be refused"),
    }
}

#[test]
fn the_declared_evidence_files_are_the_lanes_own_tests() {
    let manifest = read_required(&corpus_dir().join("manifest.json"));
    for scenario in parse_or_panic(&manifest) {
        assert!(
            scenario
                .evidence_path
                .starts_with("crates/grapt-location/tests/"),
            "scenario {:?} names evidence outside this crate's tests: {:?}",
            scenario.id,
            scenario.evidence_path
        );
    }
}
