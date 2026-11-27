//! The shared, compile-time inventory binding the R4H-L acceptance corpus to
//! the code that actually proves it.
//!
//! This module is compiled into BOTH `acceptance_corpus` and
//! `listener_contract`, which is the whole point. The corpus validator checks
//! the manifest against these constants; the declared target proves the named
//! owners are real functions in itself. Neither binary can drift from the other
//! without one of them failing, and renaming an owner test breaks compilation
//! rather than quietly passing.
//!
//! Cold-review history: R1 held this lane because the previous owner check
//! reduced ownership to the substring `fn NAME(` in an arbitrary file, so a
//! scenario could name a non-test helper in an unrelated file and stay green.
//! Everything here exists to make that class of corruption impossible to state.

#![allow(dead_code)]

/// The exact contract this corpus declares. Not a shape — this literal value.
pub const CORPUS_CONTRACT: &str = "grapt-location-acceptance-corpus/v1";
/// The exact schema version, as a JSON number rather than a string.
pub const CORPUS_SCHEMA_VERSION: u64 = 1;
pub const CORPUS_ID: &str = "r4-host-auth-foundation";
pub const CORPUS_LANE: &str = "R4H-L";
pub const CORPUS_OWNING_CRATE: &str = "grapt-location";
pub const CORPUS_ACTIONS_PATH: &str = "actions.jsonl";

/// The one integration-test target allowed to own a scenario in this corpus.
pub const CORPUS_TEST_TARGET: &str = "listener_contract";
/// The source file that target is compiled from. `tests/NAME.rs` is a cargo
/// integration-test target named NAME, so these two constants must agree.
pub const CORPUS_TEST_TARGET_PATH: &str = "crates/grapt-location/tests/listener_contract.rs";

/// Exactly the keys a v1 manifest may carry. Closed: an unknown key is a
/// refusal, so a later hand can neither smuggle a field past this owner nor
/// quietly add the `law_pack` block that must not exist here.
pub const MANIFEST_KEYS: [&str; 13] = [
    "contract",
    "schema_version",
    "id",
    "title",
    "lane",
    "owning_crate",
    "contract_note",
    "law_conformance",
    "law_pack_note",
    "scope",
    "determinism",
    "paths",
    "record_contracts",
];

/// Keys a v1 manifest may never carry.
///
/// `law_pack` is forbidden by ruling 2: the law
/// sources are not in this repository, so a hash block written here could not be
/// verified by this tree's own owner and would be fabricated evidence.
/// `oracle_commit` is forbidden because this corpus is law-conformance evidence
/// and may not claim oracle capture or parity.
pub const MANIFEST_FORBIDDEN_KEYS: [&str; 3] = ["law_pack", "oracle_commit", "oracle_receipt"];

pub const SCENARIO_KEYS: [&str; 4] = ["id", "title", "claim", "evidence"];
pub const EVIDENCE_KEYS: [&str; 3] = ["path", "test_target", "test"];

pub const LAW_CONFORMANCE_KEYS: [&str; 3] = ["basis", "oracle_status", "note"];
pub const SCOPE_KEYS: [&str; 2] = ["preserves", "intentionally_excludes"];
pub const DETERMINISM_KEYS: [&str; 4] = ["clock", "identifiers", "network", "filesystem"];

/// The number of scenarios this corpus is allowed to contain, exactly.
pub const OWNED_SCENARIO_COUNT: usize = 4;

/// The host a scenario's bind plans must name, as a shape AND a value.
///
/// A plan host is exactly one of `{"literal": "<addr>"}` or `{"name": "<host>"}`
/// — never both, never neither.
#[derive(PartialEq, Eq)]
pub enum ExpectedHost {
    Literal(&'static str),
    Name(&'static str),
}

impl ExpectedHost {
    pub fn key(&self) -> &'static str {
        match self {
            ExpectedHost::Literal(_) => "literal",
            ExpectedHost::Name(_) => "name",
        }
    }

    pub fn value(&self) -> &'static str {
        match self {
            ExpectedHost::Literal(value) | ExpectedHost::Name(value) => value,
        }
    }

    /// The address text an observation may report, when the host is literal.
    pub fn literal(&self) -> Option<&'static str> {
        match self {
            ExpectedHost::Literal(value) => Some(value),
            ExpectedHost::Name(_) => None,
        }
    }
}

/// The requested-port intent a scenario's terminal bind must carry.
///
/// `"ephemeral"` is a bare string; a fixed port is `{"fixed": <value>}`.
#[derive(PartialEq, Eq)]
pub enum ExpectedPort {
    Ephemeral,
    Fixed,
}

/// One admitted scenario, its executable owner, and everything the action log is
/// required to state for it.
///
/// `terminal_bind_acquired` is the outcome half. R1's third counterexample
/// flipped the fixed-bind scenario's terminal bind to `acquired: false` while
/// its claim still required a successful bind, and nothing noticed.
///
/// The remaining fields are the NESTED half, added after the R1 closure review
/// found a survivor: a fixed-bind `listener.observe-receipt` could claim
/// `requested_host 203.0.113.9`, `requested_port ephemeral` and
/// `observed_address 203.0.113.9:0` while the scenario claimed exact fixed
/// loopback, and the authoritative test stayed green twice. The observation was
/// only required to be a nonempty object. Every nested payload is now typed by
/// shape, by value, and against the other records in its own scenario.
pub struct OwnedScenario {
    pub id: &'static str,
    pub owner: &'static str,
    /// What the LAST `listener.bind` record in this scenario must expect.
    pub terminal_bind_acquired: bool,
    /// The classification an `expect.refusal` record must carry, when the
    /// scenario is a refusal scenario at all. `None` forbids any refusal record.
    pub refusal_classification: Option<&'static str>,
    /// The host EVERY bind plan in this scenario must name.
    pub host: ExpectedHost,
    /// The requested-port intent the TERMINAL bind must carry.
    pub terminal_port: ExpectedPort,
    /// Exactly the keys this scenario's observation must carry. Empty forbids
    /// any `listener.observe-receipt` record at all.
    pub observation_keys: &'static [&'static str],
    /// Exactly the keys this scenario's refusal must carry. Empty when the
    /// scenario carries no refusal.
    pub refusal_keys: &'static [&'static str],
}

/// The admitted four, in the admitted order.
///
/// Frozen in both directions: losing a row would shrink the corpus to whatever
/// still passed, and gaining one would be this lane inventing a behaviour
/// this lane never admitted.
pub const OWNED_SCENARIOS: [OwnedScenario; OWNED_SCENARIO_COUNT] = [
    OwnedScenario {
        id: "fixed-bind-receives-requested-port",
        owner: "a_fixed_loopback_bind_receives_exactly_the_port_it_asked_for",
        terminal_bind_acquired: true,
        refusal_classification: None,
        host: ExpectedHost::Literal(LOOPBACK),
        terminal_port: ExpectedPort::Fixed,
        observation_keys: &["requested_host", "requested_port", "observed_address"],
        refusal_keys: &[],
    },
    OwnedScenario {
        id: "ephemeral-bind-observes-assigned-port",
        owner: "an_ephemeral_bind_observes_a_real_port_and_keeps_the_ephemeral_intent",
        terminal_bind_acquired: true,
        refusal_classification: None,
        host: ExpectedHost::Literal(LOOPBACK),
        terminal_port: ExpectedPort::Ephemeral,
        observation_keys: &[
            "requested_port",
            "requested_port_asks_for",
            "observed_host",
            "observed_port_is_zero",
        ],
        refusal_keys: &[],
    },
    OwnedScenario {
        id: "bind-conflict-refused-owner-undamaged",
        owner: "a_held_port_is_refused_as_address_in_use_and_its_owner_is_undamaged",
        terminal_bind_acquired: false,
        refusal_classification: Some("address-in-use"),
        host: ExpectedHost::Literal(LOOPBACK),
        terminal_port: ExpectedPort::Fixed,
        observation_keys: &[],
        refusal_keys: &[
            "classification",
            "address",
            "classified_from",
            "classified_from_message_text",
        ],
    },
    OwnedScenario {
        id: "resolution-bound-refused-before-bind",
        owner: "a_resolution_over_its_bound_is_refused_before_any_bind_is_attempted",
        terminal_bind_acquired: false,
        refusal_classification: Some("resolution-too-many-addresses"),
        host: ExpectedHost::Name(RESOLVED_NAME),
        terminal_port: ExpectedPort::Ephemeral,
        observation_keys: &[],
        refusal_keys: &["classification", "produced", "limit", "bind_attempts"],
    },
];

/// The only literal address these scenarios may bind. Every scenario is IPv4
/// loopback by construction; the corpus states that in `scope`, and this is the
/// executable half of that statement.
pub const LOOPBACK: &str = "127.0.0.1";

/// The only name the resolution-bound scenario may resolve. It is answered by a
/// deterministic in-process resolver and never reaches the platform.
pub const RESOLVED_NAME: &str = "wide.test";

/// A refusal must be classified from the typed error kind, never from operating
/// system message text.
pub const CLASSIFIED_FROM: &str = "error-kind";

/// One action kind and the payload it is required to carry, plus the payloads it
/// may never carry.
///
/// The forbidden lists are what make the vocabulary closed rather than merely
/// enumerated: a `listener.bind` carrying resolver `answers` is a record whose
/// kind and payload disagree, and previously nothing read the payload at all.
pub struct ActionKind {
    pub kind: &'static str,
    pub required: &'static [&'static str],
    pub forbidden: &'static [&'static str],
}

pub const ACTION_KINDS: [ActionKind; 6] = [
    ActionKind {
        kind: "resolver.configure",
        required: &["answers"],
        forbidden: &["plan", "target", "expect"],
    },
    ActionKind {
        kind: "listener.bind",
        required: &["plan"],
        forbidden: &["answers", "target"],
    },
    ActionKind {
        kind: "listener.observe-receipt",
        required: &["expect"],
        forbidden: &["plan", "answers", "target"],
    },
    ActionKind {
        kind: "listener.close",
        required: &["expect"],
        forbidden: &["plan", "answers", "target"],
    },
    ActionKind {
        kind: "listener.connect",
        required: &["target", "expect"],
        forbidden: &["plan", "answers"],
    },
    ActionKind {
        kind: "expect.refusal",
        required: &["expect"],
        forbidden: &["plan", "answers", "target"],
    },
];

/// Keys a `plan` payload must carry on every `listener.bind`.
pub const PLAN_KEYS: [&str; 4] = ["host", "port", "resolution_bound", "attempt_bound"];

/// Every action record may carry these beyond its kind-specific payload.
pub const ACTION_COMMON_KEYS: [&str; 5] = ["ordinal", "scenario", "actor", "kind", "note"];

pub fn scenario_ids() -> Vec<&'static str> {
    OWNED_SCENARIOS.iter().map(|row| row.id).collect()
}

pub fn action_kind(kind: &str) -> Option<&'static ActionKind> {
    ACTION_KINDS.iter().find(|row| row.kind == kind)
}

pub fn owned_scenario(id: &str) -> Option<&'static OwnedScenario> {
    OWNED_SCENARIOS.iter().find(|row| row.id == id)
}

/// Extracts the names of the functions carrying a test attribute in a Rust
/// source file.
///
/// This is deliberately NOT a substring search. It walks lines, remembers
/// whether the previous non-blank line was `#[test]` or `#[tokio::test]`, and
/// only then reads the following `fn`/`async fn` name. A helper is therefore
/// invisible to it no matter what it is called, which is exactly the corruption
/// R1 demonstrated against the old substring check.
pub fn test_function_names(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut attributed = false;
    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        if is_test_attribute(line) {
            attributed = true;
            continue;
        }
        if attributed {
            if let Some(name) = function_name(line) {
                names.push(name);
            }
            // Any other construct after the attribute ends the run; an
            // attribute never applies across an unrelated item.
            attributed = false;
        }
    }
    names
}

fn is_test_attribute(line: &str) -> bool {
    matches!(line, "#[test]" | "#[tokio::test]")
}

/// Reads the identifier from `fn NAME(` / `async fn NAME(`, honouring an
/// optional `pub`. Returns `None` for anything that is not a function item.
fn function_name(line: &str) -> Option<String> {
    let rest = line.strip_prefix("pub ").unwrap_or(line);
    let rest = rest.strip_prefix("async ").unwrap_or(rest);
    let rest = rest.strip_prefix("fn ")?;
    let end = rest.find('(')?;
    let name = rest[..end].trim();
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some(name.to_owned())
}
