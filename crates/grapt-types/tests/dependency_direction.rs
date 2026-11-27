//! Architecture gate: `grapt-types` is the root of the crate graph and stays
//! there.
//!
//! The module doc on `lineage` claims the promoted fork law "owns **no** I/O
//! seam: it cannot read an event table". Cold review on earlier lanes
//! established that an unenforced sentence of that shape is the promise it
//! denies. The enforceable version is structural: this crate's real dependency
//! tables must be EMPTY, so there is no `rusqlite`, no `grapt-store`, no
//! `serde` and no HTTP client available to build a seam out of, whatever a
//! later author intends.
//!
//! Empty is the right law rather than an allowlist. Every OTHER crate needs an
//! allowlist because it legitimately depends on something; this one is the
//! bottom of the line-zero graph and legitimately depends on nothing. An
//! allowlist here would only be a place to quietly add the first entry.
//!
//! Dev-dependencies are deliberately NOT restricted. They cannot enter the
//! library's dependency closure, so `trybuild` and this test's own `toml` are
//! harmless — and pretending otherwise would make the gate a nuisance that
//! invites the first exception.
//!
//! The manifest is parsed STRUCTURALLY with `toml`, matching the gates in
//! `grapt-artifact`, `grapt-policy` and `grapt-schema`. A hand-rolled line
//! scanner in this repository was previously defeated by a trailing comment on
//! a section header, which made it report a clean manifest for one carrying an
//! upward dependency. A parser cannot be surprised by whitespace, comment
//! placement, key quoting, table ordering, or inline-versus-sub-table form,
//! because it never looks at lines.

use toml::Value;

/// Dependency tables that DO enter the library's closure. `dev-dependencies`
/// is deliberately absent.
const CLOSURE_TABLES: &[&str] = &["dependencies", "build-dependencies"];

/// Every dependency that would enter this crate's library closure.
fn closure_dependencies(manifest: &Value) -> Vec<String> {
    let mut found = Vec::new();
    for table in CLOSURE_TABLES {
        if let Some(deps) = manifest.get(table).and_then(Value::as_table) {
            collect(deps, &mut found);
        }
    }
    // Target-specific tables: [target.<cfg>.{dependencies,build-dependencies}].
    // A `cfg(windows)`-only seam is still a seam.
    if let Some(targets) = manifest.get("target").and_then(Value::as_table) {
        for cfg in targets.values() {
            let Some(cfg_table) = cfg.as_table() else {
                continue;
            };
            for table in CLOSURE_TABLES {
                if let Some(deps) = cfg_table.get(*table).and_then(Value::as_table) {
                    collect(deps, &mut found);
                }
            }
        }
    }
    found
}

/// Resolve each `name = spec` to its effective crate name, so a `package`
/// alias cannot rename a dependency past the gate.
fn collect(deps: &toml::value::Table, found: &mut Vec<String>) {
    for (name, spec) in deps {
        let effective = spec
            .as_table()
            .and_then(|table| table.get("package"))
            .and_then(Value::as_str)
            .unwrap_or(name.as_str());
        found.push(effective.to_owned());
    }
}

fn parse(src: &str) -> Value {
    match src.parse::<Value>() {
        Ok(value) => value,
        Err(error) => panic!("manifest parse: {error}"),
    }
}

fn manifest() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => panic!("cannot read {}: {error}", path.display()),
    };
    parse(&text)
}

#[test]
fn grapt_types_has_no_library_dependencies_at_all() {
    let violations = closure_dependencies(&manifest());
    assert!(
        violations.is_empty(),
        "grapt-types is the root of the crate graph and must declare no library \
         dependencies; found {violations:?}. A promoted foundation law that can reach \
         a store, a codec or a socket is no longer a value law."
    );
}

#[test]
fn the_gate_catches_every_bypass_form() {
    // Each smuggles a closure dependency in a different shape. If any route
    // slips through, a passing real check proves nothing.
    let cases = [
        ("plain", "[dependencies]\nrusqlite = \"0.40\"\n"),
        (
            "path dependency",
            "[dependencies]\ngrapt-store = { path = \"../grapt-store\" }\n",
        ),
        (
            "build-dependencies",
            "[build-dependencies]\nrusqlite = \"0.40\"\n",
        ),
        (
            "target-specific table",
            "[target.'cfg(windows)'.dependencies]\nrusqlite = \"0.40\"\n",
        ),
        (
            "sub-table form",
            "[dependencies.rusqlite]\nversion = \"0.40\"\n",
        ),
        (
            "package alias",
            "[dependencies]\ninnocent = { package = \"rusqlite\", version = \"0.40\" }\n",
        ),
        ("dotted key", "[dependencies]\nrusqlite.workspace = true\n"),
        // An OPTIONAL dependency is still declared in `[dependencies]`, so
        // "empty" also closes the `[features] sql = ["dep:rusqlite"]` route.
        // Features are additive and any dependent can turn one on, so a
        // feature flag would never have been access control.
        (
            "optional dependency behind a feature",
            "[dependencies]\nrusqlite = { version = \"0.40\", optional = true }\n\
             [features]\nsql = [\"dep:rusqlite\"]\n",
        ),
        // The route that defeated a hand-rolled line scanner in this repo.
        (
            "trailing comment on the section header",
            "[dependencies] # nothing to see here\nrusqlite = \"0.40\"\n",
        ),
        (
            "comment inside the table with a quoted key",
            "[dependencies]\n# a helpful note\n\"rusqlite\" = \"0.40\"\n",
        ),
    ];
    for (route, src) in cases {
        assert!(
            !closure_dependencies(&parse(src)).is_empty(),
            "the {route} route hid a library dependency from the gate"
        );
    }
}

#[test]
fn dev_dependencies_do_not_trip_the_gate() {
    // The gate must not invent a violation, or it is noise rather than a gate.
    let src = "[dev-dependencies]\ntrybuild = \"1.0.120\"\ntoml = \"0.8\"\n";
    assert!(closure_dependencies(&parse(src)).is_empty());
}

#[test]
fn grapt_types_still_inherits_the_workspace_lint_set() {
    // A crate that omits `[lints] workspace = true` silently loses
    // `unsafe_code = "forbid"`, `unused_must_use = "deny"` and the
    // `unwrap_used`/`expect_used` denials. Nothing else in the build turns red
    // when that inheritance disappears, so the manifest is the only place this
    // can be checked.
    let manifest = manifest();
    let inherited = manifest
        .get("lints")
        .and_then(Value::as_table)
        .and_then(|lints| lints.get("workspace"))
        .and_then(Value::as_bool);
    assert_eq!(
        inherited,
        Some(true),
        "grapt-types must declare `[lints] workspace = true`; without it the crate \
         drops the workspace unsafe/unwrap/expect denials with no other gate turning red"
    );
}
