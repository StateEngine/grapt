//! Architecture gate: this crate's THIRD-PARTY dependencies are admitted by
//! ruling, per table.
//!
//! # Why this gate exists at all
//!
//! `dependency_direction.rs` is a *direction* gate, and every direction gate in this
//! workspace filters on `effective.starts_with("grapt-")` — so none of them says
//! anything at all about a crates.io name. `grapt-provider` was the first crate to make
//! that hole load-bearing and took this gate under the dependency policy.
//! **`grapt-api` is the second**, and it takes the same gate for the same reason:
//! without it an author can admit their own dependency with no reviewed diff anywhere.
//!
//! # What is ruled, exactly
//!
//! The API dependency policy admits, for this candidate: `axum` under R4HA-05 with
//! `default-features = false` and the minimum resolved feature set, plus the
//! workspace-idiomatic `thiserror`. Test-only names keep their own list.
//!
//! Two names the preflight proposed are deliberately ABSENT from the production
//! list, because permission to depend is not an instruction to add an edge:
//!
//! * `tokio` — §12 admitted it for "bounded synchronization only, no `net`, no
//!   `rt`, no `macros`, no `time` unless a named use exists". No named use
//!   exists: this crate is synchronous domain law and its registry is owned
//!   behind a single `&mut`, so no synchronization primitive is reached for.
//! * `grapt-location` — A opens no socket and names no address, so it takes no
//!   location edge. That belongs to the host lane that consumes one.
//!
//! The ALLOWED sets below are the ruling transcribed. **A dependency added
//! without amending them turns this gate red**, which is the entire point.
//!
//! This gate deliberately says nothing about `grapt-*` names. Those belong to
//! `dependency_direction.rs` — one law, one carrier.

use toml::Value;

/// Externals ruled for the compiled library. Alphabetical, one per line, so a
/// diff that adds a name is a diff a reviewer cannot miss.
///
/// The last four are R4H-H's, admitted by the HTTP host contract and by the
/// governing preflight `R4HH_BOUNDED_HTTP1_HOST_PREFLIGHT_2026-08-13.md`, which states
/// the narrow feature posture each must take. Every one of these NAMES was already in
/// `Cargo.lock` at the admitted base — the stack arrives transitively through `reqwest` —
/// so what is admitted here is mostly a DIRECT edge and a feature activation. Mostly, and
/// the exception is worth stating: enabling hyper's `server` feature adds `httpdate` to
/// the lock, because `reqwest` uses hyper as a client and never activated it. A feature
/// can add a package even when every name is already present.
///
/// Two names the R4H-H preflight listed are still deliberately ABSENT:
///
/// * `tower` — H imports no `tower` public type. `RequestBodyLimit` is applied
///   directly and the Tower-to-Hyper bridge is `hyper-util`'s, so the only
///   `tower_service::Service` in play is one H never names. Adding the edge
///   because a dependency depends on it transitively is exactly the drift this
///   gate exists to catch.
/// * `http-body` — `hyper::body` re-exports `Body`, `Frame` and `SizeHint`, and
///   `axum::body` re-exports `Bytes`, so the permit-holding response body
///   implements the trait through the crates H already depends on. A second
///   direct edge to the same trait would be a version-skew hazard for nothing.
const ALLOWED_DEPENDENCIES: &[&str] = &[
    "axum",
    "hyper",
    "hyper-util",
    "serde",
    "serde_json",
    "socket2",
    "thiserror",
    "tokio",
    "tower-http",
];

/// Externals ruled for tests only. These never enter the shipped library.
const ALLOWED_DEV_DEPENDENCIES: &[&str] = &["serde_json", "tokio", "toml", "trybuild"];

const ALLOWED_BUILD_DEPENDENCIES: &[&str] = &[];

fn allowed_for(table: &str) -> &'static [&'static str] {
    match table {
        "dependencies" => ALLOWED_DEPENDENCIES,
        "dev-dependencies" => ALLOWED_DEV_DEPENDENCIES,
        "build-dependencies" => ALLOWED_BUILD_DEPENDENCIES,
        _ => &[],
    }
}

const DEP_TABLES: &[&str] = &["dependencies", "dev-dependencies", "build-dependencies"];

/// Every external dependency name this manifest declares, paired with the table
/// class that declared it.
///
/// A renamed dependency is governed by its `package` name, never by its local
/// alias — otherwise `innocent = { package = "some-other-crate" }` walks
/// straight past the list.
fn external_dependencies(manifest: &Value) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for table in DEP_TABLES {
        if let Some(deps) = manifest.get(table).and_then(Value::as_table) {
            collect(table, deps, &mut found);
        }
    }
    // Target-specific tables are the classic bypass: `[target.'cfg(windows)'
    // .dependencies]` is an ordinary dependency table the naive reader misses.
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

fn collect(table: &str, deps: &toml::value::Table, found: &mut Vec<(String, String)>) {
    for (name, spec) in deps {
        let effective = spec
            .as_table()
            .and_then(|table| table.get("package"))
            .and_then(Value::as_str)
            .unwrap_or(name.as_str());
        // `grapt-*` names are the direction gate's business, not this one's.
        if effective.starts_with("grapt-") {
            continue;
        }
        found.push((table.to_owned(), effective.to_owned()));
    }
}

fn violations(manifest: &Value) -> Vec<String> {
    external_dependencies(manifest)
        .into_iter()
        .filter(|(table, name)| !allowed_for(table).contains(&name.as_str()))
        .map(|(table, name)| format!("{table}: {name}"))
        .collect()
}

fn parse(src: &str) -> Value {
    match src.parse::<Value>() {
        Ok(value) => value,
        Err(error) => panic!("manifest parse: {error}"),
    }
}

fn manifest_text() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => panic!("cannot read {}: {error}", path.display()),
    }
}

#[test]
fn grapt_api_declares_only_ruled_external_dependencies() {
    let violations = violations(&parse(&manifest_text()));
    assert!(
        violations.is_empty(),
        "these external dependencies violate the API dependency policy: {violations:?}. Adding one requires a new \
         ruling and an amendment to this gate's ALLOWED lists — not an edit to \
         Cargo.toml alone."
    );
}

#[test]
fn the_gate_catches_every_bypass_form() {
    // A real credential/token library is the concrete thing this crate's
    // exclusions forbid permanently, so it is what every bypass route is tested
    // with.
    let cases = [
        ("plain", "[dependencies]\njsonwebtoken = \"9\"\n"),
        (
            "dev-dependencies",
            "[dev-dependencies]\njsonwebtoken = { version = \"9\" }\n",
        ),
        (
            "build-dependencies",
            "[build-dependencies]\njsonwebtoken = \"9\"\n",
        ),
        (
            "target-specific table",
            "[target.'cfg(windows)'.dependencies]\njsonwebtoken = \"9\"\n",
        ),
        (
            "sub-table form",
            "[dependencies.jsonwebtoken]\nversion = \"9\"\n",
        ),
        (
            "package alias",
            "[dependencies]\ninnocent = { package = \"jsonwebtoken\", version = \"9\" }\n",
        ),
        (
            "dotted key",
            "[dependencies]\njsonwebtoken.version = \"9\"\n",
        ),
        (
            "trailing comment on the section header",
            "[dependencies] # ordinary legal TOML\njsonwebtoken = \"9\"\n",
        ),
        (
            "comment and quoted key",
            "[dependencies]\n# note\n\"jsonwebtoken\" = \"9\"\n",
        ),
    ];
    for (route, src) in cases {
        let violations = violations(&parse(src));
        assert!(
            violations
                .iter()
                .any(|entry| entry.ends_with("jsonwebtoken")),
            "the {route} route hid a credential library from the gate: {violations:?}"
        );
    }
}

#[test]
fn a_ruled_name_in_the_wrong_table_is_still_a_violation() {
    // `trybuild` is ruled for tests and nothing else. A dev-only name that
    // migrates into the shipped library is exactly the drift a per-table list
    // exists to catch, and a single flat allowlist would miss it.
    let src = "[dependencies]\ntrybuild = \"1\"\n";
    assert_eq!(violations(&parse(src)), vec!["dependencies: trybuild"]);

    // And the converse holds: the same name in its ruled table passes.
    let src = "[dev-dependencies]\ntrybuild = \"1\"\n";
    assert!(violations(&parse(src)).is_empty());
}

#[test]
fn the_ruled_names_themselves_pass_in_their_own_tables() {
    // The same-file control. Without it, a gate that refused EVERYTHING would
    // satisfy every bypass witness above and still be worthless. It names the
    // WHOLE admitted set, R4H-H's four included, so that a name dropped from
    // `ALLOWED_DEPENDENCIES` fails here as loudly as one added.
    let src = "[dependencies]\naxum = { version = \"=0.8.9\", default-features = false }\n\
               hyper = \"1\"\nhyper-util = \"0.1\"\ntower-http = \"0.6\"\ntokio = \"1\"\n\
               socket2 = { version = \"=0.6.5\", default-features = false, features = [\"all\"] }\n\
               thiserror = \"2\"\n\
               grapt-types = { path = \"../grapt-types\" }\n\
               grapt-location = { path = \"../grapt-location\" }\n\n\
               [dev-dependencies]\nserde_json = \"1\"\ntoml = \"0.8\"\ntrybuild = \"1.0.120\"\n\
               tokio = \"1\"\n\
               grapt-types = { path = \"../grapt-types\" }\n";
    assert!(violations(&parse(src)).is_empty());
}

/// One ruled external declaration, exactly as the ruling states it.
///
/// R4H-H made the TABLE part of the identity. `tokio` is ruled twice — once for
/// the shipped library and once for tests, with a strictly larger dev feature
/// set — and a rule keyed only by name could not say that `test-util` is
/// admitted in one and forbidden in the other. Keying on the pair also keeps the
/// target-table bypass closed: a `[target.'cfg(windows)'.dependencies]` copy is
/// still COLLECTED by name, and then fails because its table is not one this
/// ruling names.
struct RuledDeclaration {
    /// The dependency table this rule governs.
    table: &'static str,
    name: &'static str,
    version: &'static str,
    /// `None` means the ruling does not speak to `default-features`, so the
    /// declaration may omit it.
    default_features: Option<bool>,
    /// The EXACT feature set. Compared as a set, so both an addition and a
    /// removal are violations.
    features: &'static [&'static str],
}

/// Exact API and HTTP host dependency declarations.
///
/// `axum`'s version is pinned EXACTLY rather than by caret. `Router::route`
/// panics rather than returning an error, so the set of inputs that panic is
/// part of `src/router.rs`'s correctness argument; a patch bump can move that
/// set and must therefore be a reviewed edit that re-measures it.
///
/// The R4H-H rows are pinned by CARET and by feature set rather than exactly,
/// and the asymmetry is deliberate: what H's correctness argument rests on is
/// which features are on, not which patch is resolved. `hyper` without
/// `server-auto`, `hyper-util` without `server-graceful`, and `tower-http` with
/// only `limit` are the architecture requirement — a silently re-enabled
/// default would hand H the automatic protocol builder it is forbidden to use,
/// and a set comparison catches that where a presence check would not.
const RULED_DECLARATIONS: &[RuledDeclaration] = &[
    RuledDeclaration {
        table: "dependencies",
        name: "axum",
        version: "=0.8.9",
        default_features: Some(false),
        features: &[],
    },
    RuledDeclaration {
        table: "dependencies",
        name: "hyper",
        version: "1",
        default_features: Some(false),
        features: &["server", "http1"],
    },
    RuledDeclaration {
        table: "dependencies",
        name: "hyper-util",
        version: "0.1",
        default_features: Some(false),
        features: &["service", "tokio"],
    },
    RuledDeclaration {
        table: "dependencies",
        // Safe keepalive probe counts and Linux TCP_USER_TIMEOUT require `all`.
        name: "socket2",
        version: "=0.6.5",
        default_features: Some(false),
        features: &["all"],
    },
    RuledDeclaration {
        table: "dependencies",
        name: "thiserror",
        version: "2",
        default_features: None,
        features: &[],
    },
    RuledDeclaration {
        table: "dependencies",
        name: "tokio",
        version: "1",
        default_features: Some(false),
        features: &["net", "rt", "sync", "time", "macros"],
    },
    RuledDeclaration {
        table: "dependencies",
        name: "tower-http",
        version: "0.6",
        default_features: Some(false),
        features: &["limit"],
    },
    RuledDeclaration {
        table: "dev-dependencies",
        name: "tokio",
        version: "1",
        default_features: Some(false),
        features: &[
            "net",
            "rt",
            "sync",
            "time",
            "macros",
            "io-util",
            "test-util",
        ],
    },
];

/// Read a dependency's declared version and feature posture, whether it is
/// written as a bare version string or as a table.
fn declaration(spec: &Value) -> (Option<&str>, Option<bool>, Vec<&str>) {
    match spec {
        Value::String(version) => (Some(version.as_str()), None, Vec::new()),
        Value::Table(table) => {
            let version = table.get("version").and_then(Value::as_str);
            let defaults = table.get("default-features").and_then(Value::as_bool);
            let features = table
                .get("features")
                .and_then(Value::as_array)
                .map(|values| values.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            (version, defaults, features)
        }
        _ => (None, None, Vec::new()),
    }
}

fn sorted(values: &[&str]) -> Vec<String> {
    let mut owned: Vec<String> = values.iter().map(|value| (*value).to_owned()).collect();
    owned.sort();
    owned
}

/// Every table in which a ruled name may be declared, including target-specific
/// ones — exactness must apply wherever the name may appear, or a
/// `[target.'cfg(windows)'.dependencies]` copy passes on its NAME while
/// carrying a different version and feature set.
fn ruled_declarations_in(manifest: &Value) -> Vec<(String, Value)> {
    let mut found = Vec::new();
    let ruled = |name: &str| RULED_DECLARATIONS.iter().any(|entry| entry.name == name);
    let mut sweep = |table: &toml::value::Table, label: &str| {
        for (name, spec) in table {
            let effective = spec
                .as_table()
                .and_then(|inner| inner.get("package"))
                .and_then(Value::as_str)
                .unwrap_or(name.as_str());
            if ruled(effective) {
                found.push((format!("{label}:{effective}"), spec.clone()));
            }
        }
    };
    for table in DEP_TABLES {
        if let Some(deps) = manifest.get(table).and_then(Value::as_table) {
            sweep(deps, table);
        }
    }
    if let Some(targets) = manifest.get("target").and_then(Value::as_table) {
        for (cfg, value) in targets {
            let Some(cfg_table) = value.as_table() else {
                continue;
            };
            for table in DEP_TABLES {
                if let Some(deps) = cfg_table.get(*table).and_then(Value::as_table) {
                    sweep(deps, &format!("target.{cfg}.{table}"));
                }
            }
        }
    }
    found
}

/// Check one declaration against the ruling, returning every disagreement.
///
/// Matched on the (table, name) PAIR. A ruled name declared in a table the
/// ruling does not name for it — including any target-specific table — is a
/// violation rather than a near miss, which is what keeps a
/// `[target.'cfg(windows)'.dependencies]` copy from passing on its name while
/// carrying a different version and feature set.
fn declaration_violations(label: &str, spec: &Value) -> Vec<String> {
    let Some((table, name)) = label.rsplit_once(':') else {
        return vec![format!("{label}: unlabelled declaration")];
    };
    let Some(ruled) = RULED_DECLARATIONS
        .iter()
        .find(|entry| entry.name == name && entry.table == table)
    else {
        return vec![format!("{label}: not a ruled declaration in this table")];
    };
    let (version, defaults, features) = declaration(spec);
    let mut problems = Vec::new();
    if version != Some(ruled.version) {
        problems.push(format!(
            "{label}: version is {version:?} but the ruling says {:?}",
            ruled.version
        ));
    }
    if let Some(expected) = ruled.default_features {
        if defaults != Some(expected) {
            problems.push(format!(
                "{label}: default-features is {defaults:?} but the ruling says {expected}"
            ));
        }
    }
    if sorted(&features) != sorted(ruled.features) {
        problems.push(format!(
            "{label}: features are {:?} but the ruling says {:?}",
            sorted(&features),
            sorted(ruled.features)
        ));
    }
    problems
}

#[test]
fn every_ruled_declaration_matches_the_ruling_exactly() {
    // The ruling is a set of exact declarations — names, versions,
    // default-feature posture and feature sets — not just a list of permitted
    // names. Feature sets are compared for EQUALITY, so a silent removal fails
    // exactly as loudly as an addition; a gate that only checked for presence
    // once stayed green while a reviewer stripped two security-relevant
    // features from a sibling crate.
    let manifest = parse(&manifest_text());
    let declarations = ruled_declarations_in(&manifest);
    assert_eq!(
        declarations.len(),
        RULED_DECLARATIONS.len(),
        "every ruled name must be declared exactly once; found {:?}",
        declarations.iter().map(|(l, _)| l).collect::<Vec<_>>()
    );
    let mut problems = Vec::new();
    for (label, spec) in &declarations {
        problems.extend(declaration_violations(label, spec));
    }
    assert!(
        problems.is_empty(),
        "these declarations disagree with the dependency policy: {problems:?}"
    );
}

#[test]
fn the_exactness_check_catches_a_drifted_declaration() {
    // The negative control for the check above. Each case is a real drift shape,
    // and a gate that only asserted presence would pass all four.
    let cases = [
        (
            "a bumped version",
            "dependencies:axum",
            "{ version = \"=0.9.0\", default-features = false }",
        ),
        (
            "default features silently re-enabled",
            "dependencies:axum",
            "{ version = \"=0.8.9\", default-features = true }",
        ),
        (
            "default-features omitted entirely",
            "dependencies:axum",
            "{ version = \"=0.8.9\" }",
        ),
        (
            "an added feature",
            "dependencies:axum",
            "{ version = \"=0.8.9\", default-features = false, features = [\"json\"] }",
        ),
        (
            "the socket keepalive feature removed",
            "dependencies:socket2",
            "{ version = \"=0.6.5\", default-features = false, features = [] }",
        ),
    ];
    for (route, label, spec) in cases {
        let value = parse(&format!("dep = {spec}\n"));
        let Some(spec_value) = value.get("dep") else {
            panic!("the {route} fixture did not parse into a dependency");
        };
        assert!(
            !declaration_violations(label, spec_value).is_empty(),
            "the {route} route drifted past the exactness check"
        );
    }

    // And the same-file control: the ruled declaration itself passes.
    let value = parse("dep = { version = \"=0.8.9\", default-features = false }\n");
    let Some(spec_value) = value.get("dep") else {
        panic!("the control fixture did not parse into a dependency");
    };
    assert!(
        declaration_violations("dependencies:axum", spec_value).is_empty(),
        "the ruled declaration itself must pass, or every case above passes for \
         the wrong reason"
    );

    let socket =
        parse("dep = { version = \"=0.6.5\", default-features = false, features = [\"all\"] }\n");
    let Some(spec) = socket.get("dep") else {
        panic!("the socket control did not parse into a dependency");
    };
    assert!(
        declaration_violations("dependencies:socket2", spec).is_empty(),
        "the admitted socket declaration must pass its exactness check"
    );
}

#[test]
fn a_ruled_declaration_is_bound_to_its_own_table() {
    // R4H-H made the table part of a rule's identity, because `tokio` is ruled
    // twice with different feature sets. These are the cases a name-only rule
    // would have admitted.
    let dev_tokio = "{ version = \"1\", default-features = false, features = \
                     [\"net\", \"rt\", \"sync\", \"time\", \"macros\", \"io-util\", \
                     \"test-util\"] }";
    let value = parse(&format!("dep = {dev_tokio}\n"));
    let Some(dev_spec) = value.get("dep") else {
        panic!("the dev tokio fixture did not parse into a dependency");
    };

    // The DEV feature set in the LIBRARY table. `test-util` is a paused-clock
    // hook, and shipping it is exactly the drift pair-keying exists to catch.
    assert!(
        !declaration_violations("dependencies:tokio", dev_spec).is_empty(),
        "the dev feature set was admitted for the shipped library"
    );

    let library = parse(
        "dep = { version = \"1\", default-features = false, features = \
         [\"net\", \"rt\", \"sync\", \"time\", \"macros\"] }\n",
    );
    let Some(library_spec) = library.get("dep") else {
        panic!("the library tokio fixture did not parse into a dependency");
    };

    // A ruled name in a target-specific table is governed by no rule at all, so
    // it is a violation rather than a near miss.
    assert!(
        !declaration_violations("target.cfg(windows).dependencies:tokio", library_spec).is_empty(),
        "a ruled name in a target-specific table was admitted on its name alone"
    );

    // The same-file controls: each set passes in the table it is ruled for, so
    // the two refusals above are about the pairing and not about tokio.
    assert!(
        declaration_violations("dependencies:tokio", library_spec).is_empty(),
        "the library tokio declaration must pass in its own table"
    );
    assert!(
        declaration_violations("dev-dependencies:tokio", dev_spec).is_empty(),
        "the dev tokio declaration must pass in its own table"
    );
}
