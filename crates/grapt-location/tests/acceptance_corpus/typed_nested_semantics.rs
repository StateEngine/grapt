/// The exact placeholder grammar. The corpus is language-neutral, so a port the
/// scenario did not choose is written as a reference to the exact earlier record
/// that observed it — never as free prose. Anything else between angle brackets
/// is not a port.
const PLACEHOLDER_PREFIX: &str = "<port observed at ordinal ";

/// Returns the ordinal a placeholder names, or `None` if the token is not
/// exactly `<port observed at ordinal N>` with `N` a plain decimal.
///
/// Leading zeros are refused so one ordinal has exactly one spelling; otherwise
/// `<…ordinal 01>` and `<…ordinal 1>` would be two tokens naming one record and
/// the equality relation below could be defeated by respelling.
fn parse_port_placeholder(token: &str) -> Option<u64> {
    let digits = token.strip_prefix(PLACEHOLDER_PREFIX)?.strip_suffix('>')?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if digits.len() > 1 && digits.starts_with('0') {
        return None;
    }
    digits.parse::<u64>().ok()
}

/// The record's own ordinal. `check_actions` already proved every record has an
/// integer ordinal before this runs, so a failure here is a contract break in
/// this file rather than fixture data — it is returned as an error rather than
/// defaulted, because a silent `0` would make every placeholder look like a
/// forward reference.
fn ordinal_of(row: &Value, scenario: &str) -> Result<u64, String> {
    row.get("ordinal").and_then(Value::as_u64).ok_or_else(|| {
        format!("scenario {scenario:?}: a record reached nested validation without an integer `ordinal`")
    })
}

/// One canonical port identity, whatever form the corpus writes it in.
///
/// The R3 closure review found the survivor this type exists to kill. The
/// previous law lived entirely inside the placeholder branch, so it bound the
/// token's SPELLING rather than the POSITION it occupies: a plain decimal was
/// range-checked and accepted whatever the scenario's terminal bind carried, and
/// `127.0.0.1:40001` walked through `observed_address`, a refusal `address` and
/// a connect `target` alike. Identity is now parsed once, in one type, and every
/// position is judged by comparing identities.
///
/// The two forms are different KINDS of identity and are never interchangeable.
/// A concrete port is not "equal" to a symbolic one for being in range, and a
/// symbolic one is never coerced to a concrete value — this corpus is
/// language-neutral and never guesses a runtime port.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PortIdentity {
    /// A reference to the exact earlier record that observed the port.
    Symbolic { ordinal: u64 },
    /// A port the scenario states outright.
    Concrete { port: u16 },
}

impl PortIdentity {
    /// How this identity is written, for a refusal message that quotes it back.
    fn describe(&self) -> String {
        match self {
            PortIdentity::Symbolic { ordinal } => format!("`{PLACEHOLDER_PREFIX}{ordinal}>`"),
            PortIdentity::Concrete { port } => format!("the literal port {port}"),
        }
    }
}

/// Parses a port token into its identity, refusing anything that is neither.
fn parse_port_identity(token: &str, context: &str) -> Result<PortIdentity, String> {
    if token.starts_with('<') || token.ends_with('>') {
        return match parse_port_placeholder(token) {
            Some(ordinal) => Ok(PortIdentity::Symbolic { ordinal }),
            None => Err(format!(
                "{context}: {token:?} is not a port; an observed port is written exactly `{PLACEHOLDER_PREFIX}N>`, naming the record that observed it"
            )),
        };
    }
    match token.parse::<u32>() {
        Ok(0) => Err(format!(
            "{context}: an observed port of 0 is the ephemeral request, never an acquired port"
        )),
        Ok(value) if value <= u32::from(u16::MAX) => Ok(PortIdentity::Concrete {
            port: value as u16,
        }),
        Ok(value) => Err(format!("{context}: {value} is not a port")),
        Err(_) => Err(format!(
            "{context}: {token:?} is neither a decimal port nor a `{PLACEHOLDER_PREFIX}N>` placeholder"
        )),
    }
}

/// What a port at some position in a scenario is permitted to be.
struct PortLaw<'a> {
    /// Ordinals of records that actually OBSERVE a port: an ephemeral
    /// `listener.bind` that was not stated to have failed. A fixed bind reuses a
    /// port rather than observing a new one, and a bind that did not acquire
    /// observed nothing at all.
    producers: &'a [u64],
    /// The ordinal of the record carrying this token. A symbolic identity may
    /// only name a port observed strictly before it.
    at: u64,
    /// The identity this scenario's terminal fixed bind carries, when it has
    /// one. Where a scenario establishes that relation, every port in it must be
    /// the same identity. An ephemeral-terminal scenario establishes no such
    /// relation and is left alone.
    terminal: Option<PortIdentity>,
}

/// A port token is judged as an identity, not as a spelling: parsed once, then
/// related to whatever identity the scenario's terminal fixed bind established.
fn check_observed_port_token(token: &str, law: &PortLaw, context: &str) -> Result<(), String> {
    let identity = parse_port_identity(token, context)?;

    // A symbolic identity must name a real, earlier producer whatever the
    // terminal relation turns out to be.
    if let PortIdentity::Symbolic { ordinal } = identity {
        if !law.producers.contains(&ordinal) {
            return Err(format!(
                "{context}: {token:?} names ordinal {ordinal}, which observes no port in this scenario; the ordinals that do are {:?}",
                law.producers
            ));
        }
        if ordinal >= law.at {
            return Err(format!(
                "{context}: {token:?} names ordinal {ordinal}, which does not precede this record at ordinal {}; a port cannot be observed after it is used",
                law.at
            ));
        }
    }

    let Some(terminal) = law.terminal else {
        // No terminal fixed bind: this scenario relates its ports to nothing,
        // and inventing a relation here would be an invented standard.
        return Ok(());
    };
    match (terminal, identity) {
        (
            PortIdentity::Symbolic { ordinal: expected },
            PortIdentity::Symbolic { ordinal: seen },
        ) => {
            if seen == expected {
                Ok(())
            } else {
                Err(format!(
                    "{context}: {token:?} is not the port this scenario's terminal fixed bind carries, {}",
                    terminal.describe()
                ))
            }
        }
        (PortIdentity::Symbolic { .. }, PortIdentity::Concrete { port }) => Err(format!(
            "{context}: the literal port {port} cannot be related to {}, which this scenario's terminal fixed bind carries; a literal is not that observed port merely by being a port, and this corpus never guesses a runtime value",
            terminal.describe()
        )),
        (PortIdentity::Concrete { port: expected }, PortIdentity::Concrete { port: seen }) => {
            if seen == expected {
                Ok(())
            } else {
                Err(format!(
                    "{context}: names port {seen}, but this scenario's terminal fixed bind carries port {expected}"
                ))
            }
        }
        (PortIdentity::Concrete { port: expected }, PortIdentity::Symbolic { .. }) => Err(format!(
            "{context}: {token:?} is an observed-port reference, but this scenario's terminal fixed bind carries the literal port {expected}; a symbolic identity is never coerced to a concrete one"
        )),
    }
}

/// `<host>:<port>`, where the host must be exactly the scenario's literal host.
fn check_address(address: &str, host: &str, law: &PortLaw, context: &str) -> Result<(), String> {
    let Some((seen_host, port)) = address.rsplit_once(':') else {
        return Err(format!(
            "{context}: address {address:?} is not `<host>:<port>`"
        ));
    };
    if seen_host != host {
        return Err(format!(
            "{context}: address {address:?} names host {seen_host:?}, but this scenario binds {host:?}"
        ));
    }
    check_observed_port_token(port, law, context)
}

/// A plan host is exactly one of `{"literal": …}` / `{"name": …}`, and must be
/// the host this scenario is admitted to bind.
fn check_plan_host(
    host: &Value,
    admitted: &support::OwnedScenario,
    context: &str,
) -> Result<(), String> {
    let Some(map) = host.as_object() else {
        return Err(format!("{context}: `plan.host` is not a JSON object"));
    };
    if map.len() != 1 {
        return Err(format!(
            "{context}: `plan.host` must carry exactly one of `literal` or `name`, found {:?}",
            map.keys().collect::<Vec<_>>()
        ));
    }
    let key = admitted.host.key();
    let expected = admitted.host.value();
    match map.get(key).and_then(Value::as_str) {
        Some(value) if value == expected => Ok(()),
        Some(value) => Err(format!(
            "{context}: `plan.host.{key}` is {value:?}, but this scenario binds {expected:?}"
        )),
        None => Err(format!(
            "{context}: `plan.host` must be {{{key:?}: {expected:?}}}, found {:?}",
            map.keys().collect::<Vec<_>>()
        )),
    }
}

/// A plan port is `"ephemeral"` or `{"fixed": <port>}`. Returns its kind.
fn check_plan_port(
    port: &Value,
    law: &PortLaw,
    context: &str,
) -> Result<support::ExpectedPort, String> {
    match port {
        Value::String(value) if value == "ephemeral" => Ok(support::ExpectedPort::Ephemeral),
        Value::String(value) => Err(format!(
            "{context}: `plan.port` is {value:?}; the only bare port intent is \"ephemeral\""
        )),
        Value::Object(map) if map.len() == 1 => match map.get("fixed") {
            // Both spellings go through the SAME identity law. A numeric fixed
            // port used to be range-checked only, so a non-terminal plan could
            // name a port unrelated to the one its scenario binds and die, if at
            // all, through an unrelated equality check elsewhere.
            Some(Value::String(token)) => {
                check_observed_port_token(token, law, &format!("{context} plan.port.fixed"))?;
                Ok(support::ExpectedPort::Fixed)
            }
            Some(Value::Number(value)) => match value.as_u64() {
                Some(0) => Err(format!(
                    "{context}: a fixed port of 0 is the ephemeral request"
                )),
                Some(seen) => {
                    check_observed_port_token(
                        &seen.to_string(),
                        law,
                        &format!("{context} plan.port.fixed"),
                    )?;
                    Ok(support::ExpectedPort::Fixed)
                }
                None => Err(format!("{context}: `plan.port.fixed` is not a port")),
            },
            _ => Err(format!(
                "{context}: `plan.port` object must be {{\"fixed\": <port>}}"
            )),
        },
        _ => Err(format!(
            "{context}: `plan.port` must be \"ephemeral\" or {{\"fixed\": <port>}}"
        )),
    }
}

/// Types every nested payload of every scenario, and relates the observation to
/// the bind plan it claims to be observing.
#[expect(clippy::too_many_lines, reason = "legacy")]
fn check_nested_semantics(collected: &[(String, String, Value)]) -> Result<(), String> {
    for admitted in support::OWNED_SCENARIOS.iter() {
        let rows: Vec<&(String, String, Value)> = collected
            .iter()
            .filter(|(scenario, _, _)| scenario == admitted.id)
            .collect();

        // Which records actually observe a port, and what the terminal fixed
        // bind carries. Both must be known before any token can be judged: a
        // placeholder is a port only if it names one of these.
        let mut producers: Vec<u64> = Vec::new();
        let mut terminal_identity: Option<PortIdentity> = None;
        for (_, kind, row) in rows.iter().copied() {
            if kind != "listener.bind" {
                continue;
            }
            let ordinal = ordinal_of(row, admitted.id)?;
            let port = row.get("plan").and_then(|plan| plan.get("port"));
            let acquired = row
                .get("expect")
                .and_then(|expect| expect.get("acquired"))
                .and_then(Value::as_bool);
            if matches!(port, Some(Value::String(value)) if value == "ephemeral")
                && acquired != Some(false)
            {
                producers.push(ordinal);
            }
            // The terminal identity is taken from BOTH spellings. A malformed
            // token yields None here rather than a guess; the same bind is
            // validated in the pass below and reports its own exact refusal.
            terminal_identity = match port {
                Some(Value::Object(map)) => match map.get("fixed") {
                    Some(Value::String(token)) => parse_port_identity(token, "").ok(),
                    Some(Value::Number(value)) => value
                        .as_u64()
                        .filter(|seen| *seen > 0 && *seen <= u64::from(u16::MAX))
                        .map(|seen| PortIdentity::Concrete { port: seen as u16 }),
                    _ => None,
                },
                _ => None,
            };
        }

        // Every plan in the scenario, typed and pinned to the admitted host.
        let mut plans: Vec<(&Value, support::ExpectedPort)> = Vec::new();
        for (index, (_, kind, row)) in rows.iter().enumerate() {
            if kind != "listener.bind" {
                continue;
            }
            let context = format!("scenario {:?} bind {index}", admitted.id);
            let law = PortLaw {
                producers: &producers,
                at: ordinal_of(row, admitted.id)?,
                terminal: terminal_identity,
            };
            let Some(plan) = row.get("plan") else {
                return Err(format!("{context}: `plan` is missing"));
            };
            let Some(host) = plan.get("host") else {
                return Err(format!("{context}: `plan.host` is missing"));
            };
            check_plan_host(host, admitted, &context)?;
            let Some(port) = plan.get("port") else {
                return Err(format!("{context}: `plan.port` is missing"));
            };
            let kind_of_port = check_plan_port(port, &law, &context)?;
            for bound in ["resolution_bound", "attempt_bound"] {
                match plan.get(bound).and_then(Value::as_u64) {
                    Some(0) | None => {
                        return Err(format!(
                            "{context}: `plan.{bound}` must be a positive integer"
                        ))
                    }
                    Some(_) => {}
                }
            }
            plans.push((port, kind_of_port));
        }

        let Some((terminal_port_value, terminal_port_kind)) = plans.last() else {
            return Err(format!(
                "scenario {:?} carries no `listener.bind` record",
                admitted.id
            ));
        };
        if *terminal_port_kind != admitted.terminal_port {
            return Err(format!(
                "scenario {:?}: its terminal bind requests a {} port, but the scenario claims a {} one",
                admitted.id,
                match terminal_port_kind {
                    support::ExpectedPort::Ephemeral => "ephemeral",
                    support::ExpectedPort::Fixed => "fixed",
                },
                match admitted.terminal_port {
                    support::ExpectedPort::Ephemeral => "ephemeral",
                    support::ExpectedPort::Fixed => "fixed",
                }
            ));
        }
        let terminal_host = rows
            .iter()
            .rfind(|(_, kind, _)| kind == "listener.bind")
            .and_then(|(_, _, row)| row.get("plan"))
            .and_then(|plan| plan.get("host"));

        // The observation, typed against that terminal plan.
        let observations: Vec<(&Value, &Value)> = rows
            .iter()
            .copied()
            .filter(|(_, kind, _)| kind == "listener.observe-receipt")
            .filter_map(|(_, _, row)| row.get("expect").map(|expect| (row, expect)))
            .collect();
        if observations.len() != usize::from(!admitted.observation_keys.is_empty()) {
            return Err(format!(
                "scenario {:?} must carry {} `listener.observe-receipt` record(s), found {}",
                admitted.id,
                usize::from(!admitted.observation_keys.is_empty()),
                observations.len()
            ));
        }
        if let Some((row, observation)) = observations.first() {
            let context = format!("scenario {:?} observation", admitted.id);
            let law = PortLaw {
                producers: &producers,
                at: ordinal_of(row, admitted.id)?,
                terminal: terminal_identity,
            };
            require_closed_keys(observation, admitted.observation_keys, &[], &context)?;
            check_observation(
                observation,
                admitted,
                terminal_host,
                terminal_port_value,
                &law,
                &context,
            )?;
        }

        // The refusal, typed against the same host.
        let refusals: Vec<(&Value, &Value)> = rows
            .iter()
            .copied()
            .filter(|(_, kind, _)| kind == "expect.refusal")
            .filter_map(|(_, _, row)| row.get("expect").map(|expect| (row, expect)))
            .collect();
        if let Some((row, refusal)) = refusals.first() {
            let context = format!("scenario {:?} refusal", admitted.id);
            let law = PortLaw {
                producers: &producers,
                at: ordinal_of(row, admitted.id)?,
                terminal: terminal_identity,
            };
            require_closed_keys(refusal, admitted.refusal_keys, &[], &context)?;
            check_refusal(refusal, admitted, &law, &context)?;
        }

        // A connect proves the holder survived; it must target this scenario's host.
        for (_, kind, row) in rows.iter() {
            if kind != "listener.connect" {
                continue;
            }
            let context = format!("scenario {:?} connect", admitted.id);
            let law = PortLaw {
                producers: &producers,
                at: ordinal_of(row, admitted.id)?,
                terminal: terminal_identity,
            };
            let target = require_str(row, "target", &context)?;
            let Some(host) = admitted.host.literal() else {
                return Err(format!(
                    "{context}: a name-resolved scenario cannot connect"
                ));
            };
            check_address(&target, host, &law, &context)?;
        }

        // A configured resolver may only answer this scenario's own name.
        for (_, kind, row) in rows.iter() {
            if kind != "resolver.configure" {
                continue;
            }
            let context = format!("scenario {:?} resolver", admitted.id);
            let Some(Value::Array(answers)) = row.get("answers") else {
                return Err(format!("{context}: `answers` must be an array"));
            };
            for answer in answers {
                let name = require_str(answer, "name", &context)?;
                if name != admitted.host.value() {
                    return Err(format!(
                        "{context}: answers for {name:?}, but this scenario resolves {:?}",
                        admitted.host.value()
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The observation must agree with the bind plan it observes.
///
/// This is the guard the R1 closure survivor walked through.
fn check_observation(
    observation: &Value,
    admitted: &support::OwnedScenario,
    terminal_host: Option<&Value>,
    terminal_port: &Value,
    law: &PortLaw,
    context: &str,
) -> Result<(), String> {
    if let Some(requested_host) = observation.get("requested_host") {
        match terminal_host {
            Some(plan_host) if requested_host == plan_host => {}
            Some(plan_host) => {
                return Err(format!(
                    "{context}: reports requested_host {requested_host}, but its terminal bind planned {plan_host}"
                ))
            }
            None => return Err(format!("{context}: there is no terminal bind host to agree with")),
        }
    }
    if let Some(requested_port) = observation.get("requested_port") {
        if requested_port != terminal_port {
            return Err(format!(
                "{context}: reports requested_port {requested_port}, but its terminal bind planned {terminal_port}"
            ));
        }
    }
    if let Some(asks_for) = observation.get("requested_port_asks_for") {
        if admitted.terminal_port != support::ExpectedPort::Ephemeral {
            return Err(format!(
                "{context}: only an ephemeral request asks for a port number"
            ));
        }
        if asks_for.as_u64() != Some(0) {
            return Err(format!(
                "{context}: an ephemeral request asks for port 0, not {asks_for}"
            ));
        }
    }
    let literal = admitted.host.literal();
    if let Some(observed) = observation.get("observed_address") {
        let Some(address) = observed.as_str() else {
            return Err(format!("{context}: `observed_address` is not a string"));
        };
        let Some(host) = literal else {
            return Err(format!(
                "{context}: a name-resolved scenario observes no literal address"
            ));
        };
        check_address(address, host, law, context)?;
    }
    if let Some(observed) = observation.get("observed_host") {
        let Some(host) = literal else {
            return Err(format!(
                "{context}: a name-resolved scenario observes no literal host"
            ));
        };
        if observed.as_str() != Some(host) {
            return Err(format!(
                "{context}: observed_host {observed} is not this scenario's host {host:?}"
            ));
        }
    }
    if let Some(zero) = observation.get("observed_port_is_zero") {
        if zero != &Value::Bool(false) {
            return Err(format!(
                "{context}: an acquired port is never zero, so `observed_port_is_zero` must be false"
            ));
        }
    }
    Ok(())
}

/// The refusal must carry its pinned classification and typed evidence.
fn check_refusal(
    refusal: &Value,
    admitted: &support::OwnedScenario,
    law: &PortLaw,
    context: &str,
) -> Result<(), String> {
    if let Some(expected) = admitted.refusal_classification {
        let seen = require_str(refusal, "classification", context)?;
        if seen != expected {
            return Err(format!(
                "{context}: classification {seen:?}, but this scenario refuses with {expected:?}"
            ));
        }
    }
    if let Some(address) = refusal.get("address") {
        let Some(address) = address.as_str() else {
            return Err(format!("{context}: `address` is not a string"));
        };
        let Some(host) = admitted.host.literal() else {
            return Err(format!(
                "{context}: a name-resolved scenario names no literal address"
            ));
        };
        check_address(address, host, law, context)?;
    }
    if refusal.get("classified_from").is_some() {
        let seen = require_str(refusal, "classified_from", context)?;
        if seen != support::CLASSIFIED_FROM {
            return Err(format!(
                "{context}: classified_from {seen:?}, but a refusal is classified from {:?}",
                support::CLASSIFIED_FROM
            ));
        }
    }
    if let Some(from_text) = refusal.get("classified_from_message_text") {
        if from_text != &Value::Bool(false) {
            return Err(format!(
                "{context}: a refusal must never be classified from message text"
            ));
        }
    }
    // The resolution bound: the produced count must actually exceed the limit,
    // and nothing may have been attempted.
    let produced = refusal.get("produced").and_then(Value::as_u64);
    let limit = refusal.get("limit").and_then(Value::as_u64);
    if let (Some(produced), Some(limit)) = (produced, limit) {
        if produced <= limit {
            return Err(format!(
                "{context}: produced {produced} does not exceed limit {limit}, so nothing was over its bound"
            ));
        }
    }
    if let Some(attempts) = refusal.get("bind_attempts") {
        if attempts.as_u64() != Some(0) {
            return Err(format!(
                "{context}: this refusal happens BEFORE any bind, so bind_attempts must be 0, not {attempts}"
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Fixtures for the negative tests. None of these touch the repository.
// ---------------------------------------------------------------------------
