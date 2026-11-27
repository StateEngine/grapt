//! The crates.io admission gate.
//!
//! `dependency_direction.rs` filters on `grapt-` and therefore says NOTHING
//! about an external name. Without this gate an author could admit any crate
//! they liked and every architecture test would stay green.
//!
//! The transport layer keeps the subset of the client's admitted names that its
//! moved code uses, with the same pin for its scheduling clock.

use std::collections::BTreeSet;
use std::path::PathBuf;

use toml::Value;

/// Every external name this crate may name, in any dependency table.
const ALLOWED: &[&str] = &["serde_json", "thiserror", "tokio", "toml"];

/// The security-patched version of the narrow clock admission, resolved in
/// the workspace lock.
///
/// Admitted as a CLOCK and nothing else: `clippy.toml` bans an ambient
/// `std::time::Instant::now` so a durable owner cannot read ambient time, and
/// names `tokio::time::Instant` as the monotonic scheduling primitive that is
/// deliberately not banned. The alternative — reusing the store adapter's
/// exemption — was refused, because a previously authorised adapter is not a
/// reusable licence for a new caller.
const TOKIO_PIN: &str = "=1.53.3";

/// The ONLY feature admitted. `time` carries `Instant` and nothing this crate
/// would otherwise reach for.
const TOKIO_FEATURES: &[&str] = &["time"];

/// A runtime, a reactor, tasks and I/O are what this admission is NOT. Enabling
/// any of them would turn a clock edge into an async architecture inside a
/// client whose dependency directions are a gated part of the design.
const TOKIO_FORBIDDEN_FEATURES: &[&str] = &[
    "rt",
    "rt-multi-thread",
    "net",
    "io-util",
    "fs",
    "process",
    "signal",
    "sync",
    "macros",
    "full",
];

/// Workspace crate prefixes: `grapt-*` and, from the owner's naming ruling,
/// `grapt-*`. Neither is a crates.io name.
const FIRST_PARTY: &[&str] = &["grapt-", "grapt-"];

const DEP_TABLES: &[&str] = &["dependencies", "dev-dependencies", "build-dependencies"];

fn manifest() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => panic!("{} must be readable: {error}", path.display()),
    };
    match text.parse::<Value>() {
        Ok(value) => value,
        Err(error) => panic!("{} must be valid TOML: {error}", path.display()),
    }
}

fn external_names(manifest: &Value) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut tables = Vec::new();
    for name in DEP_TABLES {
        if let Some(table) = manifest.get(name).and_then(Value::as_table) {
            tables.push(table);
        }
    }
    if let Some(targets) = manifest.get("target").and_then(Value::as_table) {
        for (_, target) in targets {
            for name in DEP_TABLES {
                if let Some(table) = target.get(name).and_then(Value::as_table) {
                    tables.push(table);
                }
            }
        }
    }
    for table in tables {
        for (key, definition) in table {
            let name = definition
                .as_table()
                .and_then(|t| t.get("package"))
                .and_then(Value::as_str)
                .unwrap_or(key);
            if !FIRST_PARTY.iter().any(|prefix| name.starts_with(prefix)) {
                found.insert(name.to_owned());
            }
        }
    }
    found
}

fn tokio_entry(manifest: &Value) -> toml::value::Table {
    match manifest
        .get("dependencies")
        .and_then(Value::as_table)
        .and_then(|table| table.get("tokio"))
        .and_then(Value::as_table)
    {
        Some(table) => table.clone(),
        None => panic!("tokio must be a direct dependency declared as a table"),
    }
}

#[test]
fn tokio_is_pinned_and_scoped_to_a_clock() {
    let entry = tokio_entry(&manifest());
    let version = entry.get("version").and_then(Value::as_str).unwrap_or("");
    assert_eq!(
        version, TOKIO_PIN,
        "the admission named an exact version; a bump changes the resolved          transitive set and must be a reviewed edit"
    );
    assert_eq!(
        entry.get("default-features").and_then(Value::as_bool),
        Some(false),
        "default features would pull a runtime this admission does not cover"
    );
    let features: Vec<String> = entry
        .get("features")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        features, TOKIO_FEATURES,
        "the admission is a clock: `time` and nothing else"
    );
    for forbidden in TOKIO_FORBIDDEN_FEATURES {
        assert!(
            !features.iter().any(|feature| feature == forbidden),
            "`{forbidden}` turns a clock edge into an async runtime and is refused"
        );
    }
}

#[test]
fn no_external_name_beyond_the_admitted_set() {
    let found = external_names(&manifest());
    let allowed: BTreeSet<String> = ALLOWED.iter().map(|n| (*n).to_owned()).collect();
    let refused: Vec<&String> = found.difference(&allowed).collect();
    assert!(
        refused.is_empty(),
        "these crates.io names are not admitted for grapt-client-transport: {refused:?}"
    );
}

#[test]
fn the_admitted_set_is_actually_used() {
    // A whitelist nothing matches proves nothing.
    let found = external_names(&manifest());
    let allowed: BTreeSet<String> = ALLOWED.iter().map(|n| (*n).to_owned()).collect();
    let unused: Vec<&String> = allowed.difference(&found).collect();
    assert!(
        unused.is_empty(),
        "these names are admitted but unused, so the gate measures less than it claims: {unused:?}"
    );
}
