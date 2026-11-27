//! The crates.io admission gate.
//!
//! `dependency_direction.rs` filters on `grapt-` and therefore says NOTHING
//! about an external name. Without this gate an author could admit any crate
//! they liked and every architecture test would stay green.
//!
//! Shared client and presentation dependencies stay within the admitted set,
//! with exact pins and feature limits for Unicode layout and scheduling.

use std::collections::BTreeSet;
use std::path::PathBuf;

use toml::Value;

/// Every external name this crate may name, in any dependency table.
const ALLOWED: &[&str] = &[
    // Frame-independent mention finder.
    "ignore",
    "nucleo",
    "serde",
    "serde_json",
    // Live-block content proof (B3 S2c-1a), pinned below.
    "sha2",
    "thiserror",
    "tokio",
    "toml",
    "trybuild",
    "unicode-segmentation",
    "unicode-width",
];

/// Shared Unicode layout helpers stay pinned with default features off.
const UNICODE_PINS: &[(&str, &str, &[&str])] = &[
    ("unicode-width", "=0.2.1", &[]),
    ("unicode-segmentation", "=1.13.3", &[]),
];

/// SHA-256 for the live-block content proof (B3 S2c-1a): an end is judged
/// against a running digest of every admitted byte. The version the workspace
/// lock already resolves for 18 crates, so no new package; no default features.
const SHA2_PIN: (&str, &str) = ("sha2", "=0.10.9");

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
fn sha2_is_pinned_without_default_features() {
    let (name, pin) = SHA2_PIN;
    let manifest = manifest();
    let Some(entry) = manifest
        .get("dependencies")
        .and_then(Value::as_table)
        .and_then(|table| table.get(name))
        .and_then(Value::as_table)
    else {
        panic!("{name} must be a direct dependency declared as a table");
    };
    assert_eq!(
        entry.get("version").and_then(Value::as_str),
        Some(pin),
        "{name}: the ruling admits an exact pin; a bump is a reviewed edit"
    );
    assert_eq!(
        entry.get("default-features").and_then(Value::as_bool),
        Some(false),
        "{name}: default features are not admitted"
    );
    assert!(
        entry.get("features").is_none(),
        "{name}: unreviewed features"
    );
}

#[test]
fn unicode_helpers_are_pinned_without_default_features() {
    let manifest = manifest();
    for (name, pin, admitted) in UNICODE_PINS {
        let Some(entry) = manifest
            .get("dependencies")
            .and_then(Value::as_table)
            .and_then(|table| table.get(*name))
            .and_then(Value::as_table)
        else {
            panic!("{name} must be a direct dependency declared as a table");
        };
        assert_eq!(
            entry.get("version").and_then(Value::as_str),
            Some(*pin),
            "{name}: the ruling admits an exact pin; a bump is a reviewed edit"
        );
        assert_eq!(
            entry.get("default-features").and_then(Value::as_bool),
            Some(false),
            "{name}: default features are not admitted"
        );
        let features: Vec<&str> = entry
            .get("features")
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        assert_eq!(&features, admitted, "{name}: unreviewed features");
    }
}

#[test]
fn no_external_name_beyond_the_admitted_set() {
    let found = external_names(&manifest());
    let allowed: BTreeSet<String> = ALLOWED.iter().map(|n| (*n).to_owned()).collect();
    let refused: Vec<&String> = found.difference(&allowed).collect();
    assert!(
        refused.is_empty(),
        "these crates.io names are not admitted for grapt-client: {refused:?}"
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

#[test]
fn crossterm_is_confined_to_the_admitted_terminal_crates() {
    // Terminal dependencies belong to grapt-console, which owns console events
    // and close hygiene, and to the interface crate that renders through them.
    // The interface's admission extends, by exact name, to the crates split out
    // of it (grapt-tui-transcript, grapt-tui-forms). The CLI also admits crossterm
    // for hidden key entry in the single module checked below.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let crates = match std::fs::read_dir(root.join("crates")) {
        Ok(entries) => entries,
        Err(error) => panic!("the crates directory must be readable: {error}"),
    };
    for entry in crates.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if matches!(
            name.as_str(),
            "grapt-tui" | "grapt-tui-transcript" | "grapt-tui-forms" | "grapt-console"
        ) {
            continue;
        }
        let path = entry.path().join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = text.parse::<Value>() else {
            continue;
        };
        let names = external_names(&value);
        for admitted in ["crossterm", "crossterm_winapi"] {
            if name == "grapt-cli" && admitted == "crossterm" {
                continue;
            }
            assert!(
                !names.contains(admitted),
                "{name} depends on {admitted}; only grapt-console, grapt-tui, grapt-tui-transcript, grapt-tui-forms and CLI hidden key entry are admitted"
            );
        }
    }
}

#[test]
fn grapt_cli_uses_crossterm_only_for_hidden_key_entry() {
    // The J01 widening admits raw mode for one purpose. Every other grapt-cli
    // module stays terminal-independent.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../grapt-cli/src");
    let mut pending = vec![root.clone()];
    let mut users = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => panic!("{} must be readable: {error}", directory.display()),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let text = match std::fs::read_to_string(&path) {
                    Ok(text) => text,
                    Err(error) => panic!("{} must be readable: {error}", path.display()),
                };
                if text.contains("crossterm") {
                    users.push(
                        path.strip_prefix(&root)
                            .map(|relative| relative.to_path_buf())
                            .unwrap_or_default(),
                    );
                }
            }
        }
    }
    assert_eq!(
        users,
        [PathBuf::from("client/terminal/secret_input.rs")],
        "crossterm escaped its one grapt-cli module"
    );
}
