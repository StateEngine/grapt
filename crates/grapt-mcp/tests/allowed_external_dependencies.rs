//! Architecture gate: this crate's THIRD-PARTY dependencies are admitted per
//! table, by name.
//!
//! `dependency_direction.rs` filters on `grapt-` names and therefore says
//! nothing about a crates.io dependency. That hole matters here more than it
//! does in a leaf domain crate: an MCP client is exactly the place someone
//! would reach for an HTTP client, a process spawner, an async runtime or a
//! ready-made protocol SDK. This crate must acquire none of them — it is pure,
//! and every one of those capabilities belongs to an owner above it.
//!
//! The lists below are the exact current set. A dependency added without
//! amending them turns this gate red, which is the point: an author cannot
//! admit their own dependency.

use toml::Value;

/// Externals in the compiled library. Alphabetical, one per line, so a diff
/// that adds a name is a diff a reviewer cannot miss.
const ALLOWED_DEPENDENCIES: &[&str] = &["serde_json", "thiserror", "url"];

/// Externals for tests only. These never enter the shipped library.
const ALLOWED_DEV_DEPENDENCIES: &[&str] = &["toml"];

/// Nothing builds this crate at build time, and nothing may start.
const ALLOWED_BUILD_DEPENDENCIES: &[&str] = &[];

const DEP_TABLES: &[&str] = &["dependencies", "dev-dependencies", "build-dependencies"];

fn allowed_for(table: &str) -> &'static [&'static str] {
    match table {
        "dependencies" => ALLOWED_DEPENDENCIES,
        "dev-dependencies" => ALLOWED_DEV_DEPENDENCIES,
        "build-dependencies" => ALLOWED_BUILD_DEPENDENCIES,
        _ => &[],
    }
}

/// A renamed dependency is governed by its `package` name, never by its local
/// alias — otherwise `innocent = { package = "some-http-client" }` walks
/// straight past the list.
fn collect(table: &str, deps: &toml::value::Table, found: &mut Vec<(String, String)>) {
    for (name, spec) in deps {
        let effective = spec
            .as_table()
            .and_then(|table| table.get("package"))
            .and_then(Value::as_str)
            .unwrap_or(name.as_str());
        if effective.starts_with("grapt-") {
            continue;
        }
        found.push((table.to_owned(), effective.to_owned()));
    }
}

fn external_dependencies(manifest: &Value) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for table in DEP_TABLES {
        if let Some(deps) = manifest.get(table).and_then(Value::as_table) {
            collect(table, deps, &mut found);
        }
    }
    // Target-specific tables are the classic bypass: `[target.'cfg(windows)'
    // .dependencies]` is an ordinary dependency table a naive reader misses.
    if let Some(targets) = manifest.get("target").and_then(Value::as_table) {
        for cfg in targets.values() {
            let Some(cfg_table) = cfg.as_table() else {
                continue;
            };
            for table in DEP_TABLES {
                if let Some(deps) = cfg_table.get(*table).and_then(Value::as_table) {
                    collect(table, deps, &mut found);
                }
            }
        }
    }
    found
}

fn manifest() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => panic!("cannot read {}: {error}", path.display()),
    };
    match text.parse::<Value>() {
        Ok(value) => value,
        Err(error) => panic!("manifest parse: {error}"),
    }
}

#[test]
fn every_third_party_dependency_is_admitted_by_name_and_table() {
    let violations: Vec<String> = external_dependencies(&manifest())
        .into_iter()
        .filter(|(table, name)| !allowed_for(table).contains(&name.as_str()))
        .map(|(table, name)| format!("{table}: {name}"))
        .collect();
    assert_eq!(
        violations,
        Vec::<String>::new(),
        "an unruled third-party dependency was added to the MCP protocol domain"
    );
}

#[test]
fn the_protocol_domain_takes_no_runtime_transport_or_process_dependency() {
    // Named rather than left implicit: these are the exact capabilities this
    // crate exists NOT to hold, and the names an author would reach for first.
    const FORBIDDEN: &[&str] = &[
        "async-std",
        "hyper",
        "reqwest",
        "rmcp",
        "smol",
        "std-process",
        "tokio",
        "ureq",
    ];
    let taken: Vec<String> = external_dependencies(&manifest())
        .into_iter()
        .filter(|(_, name)| FORBIDDEN.contains(&name.as_str()))
        .map(|(table, name)| format!("{table}: {name}"))
        .collect();
    assert_eq!(
        taken,
        Vec::<String>::new(),
        "the MCP protocol domain took a transport, runtime or process dependency"
    );
}
