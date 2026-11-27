fn manifest_fixture() -> String {
    let scenarios: Vec<String> = support::OWNED_SCENARIOS
        .iter()
        .map(|row| {
            format!(
                r#"{{"id":"{}","title":"t","claim":"c","evidence":{{"path":"{}","test_target":"{}","test":"{}"}}}}"#,
                row.id,
                support::CORPUS_TEST_TARGET_PATH,
                support::CORPUS_TEST_TARGET,
                row.owner
            )
        })
        .collect();
    format!(
        r#"{{"contract":"{}","schema_version":{},"id":"{}","title":"t","lane":"{}","owning_crate":"{}","contract_note":"n","law_conformance":{{"basis":"b","oracle_status":"o","note":"n"}},"law_pack_note":"n","scope":{{"preserves":["p"],"intentionally_excludes":["e"]}},"determinism":{{"clock":"c","identifiers":"i","network":"n","filesystem":"f"}},"paths":{{"actions":"{}"}},"record_contracts":{{"actions":"a"}},"scenarios":[{}]}}"#,
        support::CORPUS_CONTRACT,
        support::CORPUS_SCHEMA_VERSION,
        support::CORPUS_ID,
        support::CORPUS_LANE,
        support::CORPUS_OWNING_CRATE,
        support::CORPUS_ACTIONS_PATH,
        scenarios.join(",")
    )
}

/// The fixed port the control fixture pretends was observed.
const FIXTURE_PORT: &str = "40001";

fn fixture_host(row: &support::OwnedScenario) -> String {
    format!(r#"{{"{}":"{}"}}"#, row.host.key(), row.host.value())
}

fn fixture_port(row: &support::OwnedScenario) -> String {
    match row.terminal_port {
        support::ExpectedPort::Ephemeral => r#""ephemeral""#.to_owned(),
        support::ExpectedPort::Fixed => format!(r#"{{"fixed":"{FIXTURE_PORT}"}}"#),
    }
}

fn fixture_observation_value(key: &str, row: &support::OwnedScenario) -> String {
    match key {
        "requested_host" => fixture_host(row),
        "requested_port" => fixture_port(row),
        "requested_port_asks_for" => "0".to_owned(),
        "observed_address" => format!(r#""{}:{FIXTURE_PORT}""#, support::LOOPBACK),
        "observed_host" => format!(r#""{}""#, support::LOOPBACK),
        "observed_port_is_zero" => "false".to_owned(),
        other => panic!("the control fixture has no value for observation key {other:?}"),
    }
}

fn fixture_refusal_value(key: &str, classification: &str) -> String {
    match key {
        "classification" => format!(r#""{classification}""#),
        "address" => format!(r#""{}:{FIXTURE_PORT}""#, support::LOOPBACK),
        "classified_from" => format!(r#""{}""#, support::CLASSIFIED_FROM),
        "classified_from_message_text" => "false".to_owned(),
        "produced" => "3".to_owned(),
        "limit" => "2".to_owned(),
        "bind_attempts" => "0".to_owned(),
        other => panic!("the control fixture has no value for refusal key {other:?}"),
    }
}

fn fixture_object(pairs: &[(String, String)]) -> String {
    let body: Vec<String> = pairs
        .iter()
        .map(|(key, value)| format!(r#""{key}":{value}"#))
        .collect();
    format!("{{{}}}", body.join(","))
}

/// A control log that satisfies every admitted scenario completely: its host,
/// its terminal port intent, its observation key set and its refusal key set.
///
/// Built from the inventory rather than hand-written, so it cannot drift from
/// what the validator requires — if a scenario gains a pinned key, this fixture
/// gains it too or panics naming the key.
fn actions_fixture() -> String {
    let mut lines = Vec::new();
    let mut ordinal = 0;
    for row in support::OWNED_SCENARIOS.iter() {
        if let support::ExpectedHost::Name(name) = row.host {
            ordinal += 1;
            lines.push(format!(
                r#"{{"ordinal":{ordinal},"scenario":"{}","actor":"harness","kind":"resolver.configure","answers":[{{"name":"{name}","addresses":["{}"]}}]}}"#,
                row.id,
                support::LOOPBACK
            ));
        }
        ordinal += 1;
        lines.push(format!(
            r#"{{"ordinal":{ordinal},"scenario":"{}","actor":"harness","kind":"listener.bind","plan":{{"host":{},"port":{},"resolution_bound":4,"attempt_bound":4}},"expect":{{"acquired":{}}}}}"#,
            row.id,
            fixture_host(row),
            fixture_port(row),
            row.terminal_bind_acquired
        ));
        if !row.observation_keys.is_empty() {
            let pairs: Vec<(String, String)> = row
                .observation_keys
                .iter()
                .map(|key| ((*key).to_owned(), fixture_observation_value(key, row)))
                .collect();
            ordinal += 1;
            lines.push(format!(
                r#"{{"ordinal":{ordinal},"scenario":"{}","actor":"harness","kind":"listener.observe-receipt","expect":{}}}"#,
                row.id,
                fixture_object(&pairs)
            ));
        }
        if let Some(classification) = row.refusal_classification {
            let pairs: Vec<(String, String)> = row
                .refusal_keys
                .iter()
                .map(|key| {
                    (
                        (*key).to_owned(),
                        fixture_refusal_value(key, classification),
                    )
                })
                .collect();
            ordinal += 1;
            lines.push(format!(
                r#"{{"ordinal":{ordinal},"scenario":"{}","actor":"harness","kind":"expect.refusal","expect":{}}}"#,
                row.id,
                fixture_object(&pairs)
            ));
        }
    }
    lines.join("\n")
}

/// A control log written the way the real corpus writes a port it did not
/// choose: as `<port observed at ordinal N>` naming the exact earlier record
/// that observed it.
///
/// `actions_fixture` uses a literal decimal port, so it never exercises the
/// placeholder path at all — which is precisely why the R2 closure survivor
/// lived there unwitnessed. A fixed-terminal scenario here borrows **two** ports
/// before its fixed bind, so "names a producing ordinal" and "names the
/// terminal bind's own producing ordinal" are separable failures: naming the
/// first borrow is well-formed, precedes, and is a real producer, yet is still
/// not the port the terminal bind carries.
struct PlaceholderControl {
    log: String,
    /// The token the terminal fixed bind carries.
    terminal: String,
    /// A different producer in the same scenario: equally well-formed, equally
    /// real, and equally preceding — but not the terminal bind's port.
    other: String,
    /// The exact emitted fragment at each position a port can occupy, so a
    /// negative test corrupts the position it means rather than whichever one
    /// happens to appear first. Each belongs to whichever scenario carries it.
    observed_address: String,
    refusal_address: String,
    connect_target: String,
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn placeholder_control() -> PlaceholderControl {
    let mut lines = Vec::new();
    let mut ordinal = 0;
    let mut terminal = String::new();
    let mut other = String::new();
    let mut observed_address = String::new();
    let mut refusal_address = String::new();
    let mut connect_target = String::new();
    for row in support::OWNED_SCENARIOS.iter() {
        if let support::ExpectedHost::Name(name) = row.host {
            ordinal += 1;
            lines.push(format!(
                r#"{{"ordinal":{ordinal},"scenario":"{}","actor":"harness","kind":"resolver.configure","answers":[{{"name":"{name}","addresses":["{}"]}}]}}"#,
                row.id,
                support::LOOPBACK
            ));
        }
        let token = match row.terminal_port {
            support::ExpectedPort::Fixed => {
                let mut borrowed = Vec::new();
                for _ in 0..2 {
                    ordinal += 1;
                    lines.push(format!(
                        r#"{{"ordinal":{ordinal},"scenario":"{}","actor":"harness","kind":"listener.bind","plan":{{"host":{},"port":"ephemeral","resolution_bound":4,"attempt_bound":4}}}}"#,
                        row.id,
                        fixture_host(row)
                    ));
                    borrowed.push(format!("{PLACEHOLDER_PREFIX}{ordinal}>"));
                }
                if terminal.is_empty() {
                    other = borrowed[0].clone();
                    terminal = borrowed[1].clone();
                }
                Some(borrowed[1].clone())
            }
            support::ExpectedPort::Ephemeral => None,
        };
        let plan_port = match &token {
            Some(token) => format!(r#"{{"fixed":"{token}"}}"#),
            None => r#""ephemeral""#.to_owned(),
        };
        ordinal += 1;
        lines.push(format!(
            r#"{{"ordinal":{ordinal},"scenario":"{}","actor":"harness","kind":"listener.bind","plan":{{"host":{},"port":{plan_port},"resolution_bound":4,"attempt_bound":4}},"expect":{{"acquired":{}}}}}"#,
            row.id,
            fixture_host(row),
            row.terminal_bind_acquired
        ));
        if !row.observation_keys.is_empty() {
            let pairs: Vec<(String, String)> = row
                .observation_keys
                .iter()
                .map(|key| {
                    (
                        (*key).to_owned(),
                        placeholder_observation_value(key, row, token.as_deref()),
                    )
                })
                .collect();
            if observed_address.is_empty() {
                if let Some((key, value)) = pairs.iter().find(|(key, _)| key == "observed_address")
                {
                    observed_address = format!(r#""{key}":{value}"#);
                }
            }
            ordinal += 1;
            lines.push(format!(
                r#"{{"ordinal":{ordinal},"scenario":"{}","actor":"harness","kind":"listener.observe-receipt","expect":{}}}"#,
                row.id,
                fixture_object(&pairs)
            ));
        }
        if let Some(classification) = row.refusal_classification {
            let pairs: Vec<(String, String)> = row
                .refusal_keys
                .iter()
                .map(|key| {
                    (
                        (*key).to_owned(),
                        placeholder_refusal_value(key, classification, token.as_deref()),
                    )
                })
                .collect();
            if refusal_address.is_empty() && token.is_some() {
                if let Some((key, value)) = pairs.iter().find(|(key, _)| key == "address") {
                    refusal_address = format!(r#""{key}":{value}"#);
                }
            }
            ordinal += 1;
            lines.push(format!(
                r#"{{"ordinal":{ordinal},"scenario":"{}","actor":"harness","kind":"expect.refusal","expect":{}}}"#,
                row.id,
                fixture_object(&pairs)
            ));
        }
        // A connect proves the holder survived a conflict, and is the third
        // position a port can occupy. The generated control carried none, so
        // that position had no in-process witness at all.
        if let (Some(token), Some(host)) = (token.as_deref(), row.host.literal()) {
            let target = format!(r#""target":"{host}:{token}""#);
            if connect_target.is_empty() {
                connect_target = target.clone();
            }
            ordinal += 1;
            lines.push(format!(
                r#"{{"ordinal":{ordinal},"scenario":"{}","actor":"harness","kind":"listener.connect",{target},"expect":{{"connection_completed":true}}}}"#,
                row.id
            ));
        }
    }
    assert!(
        !terminal.is_empty()
            && !other.is_empty()
            && !observed_address.is_empty()
            && !refusal_address.is_empty()
            && !connect_target.is_empty(),
        "the placeholder control must exercise every position a port can occupy"
    );
    PlaceholderControl {
        log: lines.join("\n"),
        terminal,
        other,
        observed_address,
        refusal_address,
        connect_target,
    }
}

fn placeholder_observation_value(
    key: &str,
    row: &support::OwnedScenario,
    token: Option<&str>,
) -> String {
    match (key, token) {
        ("requested_port", Some(token)) => format!(r#"{{"fixed":"{token}"}}"#),
        ("observed_address", Some(token)) => format!(r#""{}:{token}""#, support::LOOPBACK),
        _ => fixture_observation_value(key, row),
    }
}

fn placeholder_refusal_value(key: &str, classification: &str, token: Option<&str>) -> String {
    match (key, token) {
        ("address", Some(token)) => format!(r#""{}:{token}""#, support::LOOPBACK),
        _ => fixture_refusal_value(key, classification),
    }
}

fn parse_or_panic(src: &str) -> Vec<Scenario> {
    match parse_manifest(src) {
        Ok(scenarios) => scenarios,
        Err(error) => panic!("fixture manifest must parse: {error}"),
    }
}

/// Swaps the symbolic token inside an emitted fragment for a plain decimal,
/// leaving the position and every other byte alone. This is the exact shape of
/// the R3 closure survivor: same position, same host, a literal port.
fn with_literal_port(fragment: &str, literal: &str) -> String {
    let Some(start) = fragment.find(PLACEHOLDER_PREFIX) else {
        panic!("the fragment {fragment:?} must carry a placeholder to replace");
    };
    let Some(offset) = fragment[start..].find('>') else {
        panic!("the placeholder in {fragment:?} must be closed");
    };
    let end = start + offset;
    format!("{}{literal}{}", &fragment[..start], &fragment[end + 1..])
}

/// Replaces the first occurrence of `from` with `to`, refusing if absent so a
/// negative test can never silently assert against an unmodified fixture.
fn corrupt(src: &str, from: &str, to: &str) -> String {
    assert!(
        src.contains(from),
        "the corruption target {from:?} is not present in the fixture"
    );
    src.replacen(from, to, 1)
}

// ---------------------------------------------------------------------------
// The authoritative test.
// ---------------------------------------------------------------------------
