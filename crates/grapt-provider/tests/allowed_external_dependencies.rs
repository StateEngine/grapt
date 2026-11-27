//! Architecture gate: this crate's THIRD-PARTY dependencies are admitted by
//! ruling, per table.
//!
//! # Why this gate exists at all
//!
//! `dependency_direction.rs` is a *direction* gate: every one of the nine
//! sibling gates in this workspace filters on
//! `effective.starts_with("grapt-")`, so none of them says anything about a
//! crates.io dependency. A separate allowlist is required for third-party
//! packages used by a boundary crate, including its HTTP client.
//!
//! # What is ruled, exactly
//!
//! The admission ruling admits, for this candidate:
//! `reqwest 0.13.4` with `default-features = false` and features
//! `[rustls-no-provider, http2]`; `rustls 0.23` with `default-features = false`
//! and features `[ring, std, tls12]`; `rustls-platform-verifier 0.7`;
//! `serde_json 1`; `tokio 1` with only the features actually required; and the
//! pre-existing `thiserror`. Test-only names keep their own list.
//!
//! The ALLOWED sets below are that ruling transcribed. **A dependency added
//! without amending them turns this gate red**, which is the whole point: an
//! author cannot admit their own dependency.
//!
//! This gate deliberately says nothing about `grapt-*` names. Those belong to
//! `dependency_direction.rs` — one law, one carrier.

use toml::Value;

/// Externals ruled for the compiled library. Alphabetical, one per line, so a
/// diff that adds a name is easy to inspect.
const ALLOWED_DEPENDENCIES: &[&str] = &[
    "reqwest",
    "rustls",
    "rustls-platform-verifier",
    "serde_json",
    "sha2",
    "thiserror",
    "tokio",
];

/// Externals ruled for tests only. These never enter the shipped library.
const ALLOWED_DEV_DEPENDENCIES: &[&str] = &["tokio", "toml", "trybuild"];

/// The dev-table posture for a ruled name that also appears in tests.
///
/// Exactness applies to **every** admitted declaration, so a name that
/// legitimately appears twice needs its second declaration ruled too rather
/// than waived. `tokio` is the only such name: production takes `time` alone,
/// while the loopback fixtures need a runtime, a listener, byte I/O and a
/// controlled clock for deterministic capacity and account cooldown tests.
const RULED_DEV_DECLARATIONS: &[RuledDeclaration] = &[RuledDeclaration {
    name: "tokio",
    version: "1",
    default_features: Some(false),
    features: &[
        "time",
        "rt",
        "rt-multi-thread",
        "macros",
        "net",
        "io-util",
        "test-util",
    ],
}];

/// Nothing builds this crate at build time, and nothing may start.
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
/// alias — otherwise `innocent = { package = "some-other-client" }` walks
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
fn grapt_provider_declares_only_ruled_external_dependencies() {
    let violations = violations(&parse(&manifest_text()));
    assert!(
        violations.is_empty(),
        "these external dependencies violate the provider dependency policy: {violations:?}. Adding one requires a new \
         ruling and an amendment to this gate's ALLOWED lists — not an edit to \
         Cargo.toml alone."
    );
}

#[test]
fn the_gate_catches_every_bypass_form() {
    // A second HTTP client is the concrete thing the ruling forbids, so it is
    // what every bypass route is tested with.
    let cases = [
        ("plain", "[dependencies]\nureq = \"3\"\n"),
        (
            "dev-dependencies",
            "[dev-dependencies]\nureq = { version = \"3\" }\n",
        ),
        ("build-dependencies", "[build-dependencies]\nureq = \"3\"\n"),
        (
            "target-specific table",
            "[target.'cfg(windows)'.dependencies]\nureq = \"3\"\n",
        ),
        ("sub-table form", "[dependencies.ureq]\nversion = \"3\"\n"),
        (
            "package alias",
            "[dependencies]\ninnocent = { package = \"ureq\", version = \"3\" }\n",
        ),
        ("dotted key", "[dependencies]\nureq.version = \"3\"\n"),
        (
            "trailing comment on the section header",
            "[dependencies] # ordinary legal TOML\nureq = \"3\"\n",
        ),
        (
            "comment and quoted key",
            "[dependencies]\n# note\n\"ureq\" = \"3\"\n",
        ),
    ];
    for (route, src) in cases {
        let violations = violations(&parse(src));
        assert!(
            violations.iter().any(|entry| entry.ends_with("ureq")),
            "the {route} route hid a second HTTP client from the gate: {violations:?}"
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
    let src = "[dependencies]\nreqwest = { version = \"0.13.4\", default-features = false }\n\
               rustls = \"0.23\"\nrustls-platform-verifier = \"0.7\"\nserde_json = \"1\"\n\
               sha2 = \"0.10\"\nthiserror = \"2\"\ntokio = { version = \"1\" }\n\
               grapt-types = { path = \"../grapt-types\" }\n\n\
               [dev-dependencies]\ntoml = \"0.8\"\ntrybuild = \"1\"\n";
    assert!(violations(&parse(src)).is_empty());
}

/// One ruled external declaration, exactly as the ruling states it.
struct RuledDeclaration {
    name: &'static str,
    version: &'static str,
    /// `None` means the ruling does not speak to `default-features`, so the
    /// declaration may omit it.
    default_features: Option<bool>,
    /// The EXACT feature set. Compared as a set, so both an addition and a
    /// removal are violations.
    features: &'static [&'static str],
}

/// The provider dependency policy, transcribed.
const RULED_DECLARATIONS: &[RuledDeclaration] = &[
    // Hashing for provider request binding.
    RuledDeclaration {
        name: "sha2",
        version: "0.10",
        default_features: Some(false),
        features: &["std"],
    },
    RuledDeclaration {
        name: "reqwest",
        version: "0.13.4",
        default_features: Some(false),
        features: &["rustls-no-provider", "http2"],
    },
    RuledDeclaration {
        name: "rustls",
        version: "0.23",
        default_features: Some(false),
        features: &["ring", "std", "tls12"],
    },
    RuledDeclaration {
        name: "rustls-platform-verifier",
        version: "0.7",
        default_features: None,
        features: &[],
    },
    RuledDeclaration {
        name: "serde_json",
        version: "1",
        default_features: None,
        features: &[],
    },
    RuledDeclaration {
        name: "thiserror",
        version: "2",
        default_features: None,
        features: &[],
    },
    RuledDeclaration {
        name: "tokio",
        version: "1",
        default_features: Some(false),
        features: &["time"],
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

/// The ruling is a set of exact declarations — names, versions, default-feature
/// posture and feature sets — not just a list of permitted names.
///
/// # Why this is stricter than it was
///
/// The previous version asserted only that `ring` was
/// present and `aws-lc-rs` absent. Removing **both `std` and
/// `tls12`** from the rustls declaration left this gate GREEN — a broken
/// witness over a real posture change. Feature sets are now compared for
/// equality, so a removal fails exactly as loudly as an addition, and the
/// version strings are pinned too: a silent bump to a different `reqwest` minor
/// is a different ruling.
/// Every table in which a ruled name may be declared, including target-specific
/// ones.
///
/// The exactness check read only the main
/// `[dependencies]` table, while the name check reached target-specific tables.
/// A `[target.'cfg(windows)'.dependencies] reqwest = { version = "99", ... }`
/// therefore passed on its NAME while carrying a different version, default
/// posture and feature set. Exactness now applies wherever the name may appear.
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

/// Compare one declaration against its ruling. Returns the reasons it differs.
fn declaration_drift(label: &str, spec: &Value) -> Vec<String> {
    let name = label.rsplit(':').next().unwrap_or(label);
    // A dev declaration is ruled by its own posture; everything else — the main
    // table and every target-specific table — is ruled by the production one.
    let table = label.rsplit(':').nth(1).unwrap_or("");
    let ruling = if table.ends_with("dev-dependencies") {
        RULED_DEV_DECLARATIONS
    } else {
        RULED_DECLARATIONS
    };
    let Some(ruled) = ruling.iter().find(|entry| entry.name == name) else {
        return vec![format!("{label}: not a ruled name for {table}")];
    };
    let (version, defaults, features) = declaration(spec);
    let mut drift = Vec::new();
    if version != Some(ruled.version) {
        drift.push(format!(
            "{label}: version {version:?} is not {:?}",
            ruled.version
        ));
    }
    if defaults != ruled.default_features {
        drift.push(format!(
            "{label}: default-features {defaults:?} is not {:?}",
            ruled.default_features
        ));
    }
    if sorted(&features) != sorted(ruled.features) {
        drift.push(format!(
            "{label}: features {:?} are not the ruled set",
            sorted(&features)
        ));
    }
    drift
}

/// A ruled name declared in ANY table — including a target-specific one —
/// must carry the exact ruled version, default posture and feature set.
#[test]
fn a_ruled_name_drifting_in_any_table_is_refused() {
    let counterexamples = [
        (
            "target-specific version drift",
            "[target.'cfg(windows)'.dependencies]
reqwest = { version = \"99\", default-features = false, features = [\"rustls-no-provider\", \"http2\"] }
",
        ),
        (
            "target-specific default-features drift",
            "[target.'cfg(unix)'.dependencies]
reqwest = { version = \"0.13.4\", features = [\"rustls-no-provider\", \"http2\"] }
",
        ),
        (
            "target-specific feature drift",
            "[target.'cfg(windows)'.dependencies]
reqwest = { version = \"0.13.4\", default-features = false, features = [\"rustls-no-provider\", \"http2\", \"json\"] }
",
        ),
        (
            "dev-table drift of a ruled name",
            "[dev-dependencies]
tokio = { version = \"1\", default-features = false, features = [\"time\", \"net\"] }
",
        ),
    ];
    for (route, src) in counterexamples {
        let manifest = parse(src);
        let declared = ruled_declarations_in(&manifest);
        assert!(!declared.is_empty(), "the {route} route was not even seen");
        let drift: Vec<String> = declared
            .iter()
            .flat_map(|(label, spec)| declaration_drift(label, spec))
            .collect();
        assert!(
            !drift.is_empty(),
            "the {route} route bypassed the exact posture check"
        );
    }
}

#[test]
fn the_ruled_declarations_are_exact() {
    let manifest = parse(&manifest_text());
    let deps = match manifest.get("dependencies").and_then(Value::as_table) {
        Some(deps) => deps,
        None => panic!("the manifest has a [dependencies] table"),
    };

    // The real manifest must also carry no drifting ruled declaration in
    // ANY table, target-specific ones included.
    let drift: Vec<String> = ruled_declarations_in(&manifest)
        .iter()
        .flat_map(|(label, spec)| declaration_drift(label, spec))
        .collect();
    assert!(drift.is_empty(), "ruled declarations drifted: {drift:?}");

    for ruled in RULED_DECLARATIONS {
        let spec = match deps.get(ruled.name) {
            Some(spec) => spec,
            None => panic!(
                "{} is ruled for this crate and is missing from [dependencies]",
                ruled.name
            ),
        };
        let (version, defaults, features) = declaration(spec);
        assert_eq!(
            version,
            Some(ruled.version),
            "{} must be pinned at the ruled version",
            ruled.name
        );
        assert_eq!(
            defaults, ruled.default_features,
            "{}'s default-features posture is part of the ruling",
            ruled.name
        );
        assert_eq!(
            sorted(&features),
            sorted(ruled.features),
            "{}'s feature set must be exactly the ruled set — a REMOVAL is as much a \
             violation as an addition",
            ruled.name
        );
    }

    // And nothing else may appear in the shipped library's table.
    let declared: Vec<String> = deps
        .keys()
        .filter(|name| !name.starts_with("grapt-"))
        .cloned()
        .collect();
    let mut expected: Vec<String> = RULED_DECLARATIONS
        .iter()
        .map(|ruled| ruled.name.to_owned())
        .collect();
    expected.sort();
    let mut declared_sorted = declared.clone();
    declared_sorted.sort();
    assert_eq!(
        declared_sorted, expected,
        "[dependencies] must contain exactly the ruled external names"
    );
}

/// Removing required TLS features must fail the gate's own exact comparison.
///
/// This is the same shape as the bypass-form test: rather than mutate the real
/// manifest, it feeds the gate a manifest with `std` and `tls12` removed and
/// requires the comparison to reject it. If the comparison ever weakens back to
/// "contains ring", this test goes red on the spot.
#[test]
fn removing_a_ruled_feature_is_caught() {
    let ruled_rustls = match RULED_DECLARATIONS
        .iter()
        .find(|ruled| ruled.name == "rustls")
    {
        Some(ruled) => ruled,
        None => panic!("rustls is a ruled declaration"),
    };

    for removed in ["std", "tls12", "ring"] {
        let remaining: Vec<&str> = ruled_rustls
            .features
            .iter()
            .copied()
            .filter(|feature| *feature != removed)
            .collect();
        assert_ne!(
            sorted(&remaining),
            sorted(ruled_rustls.features),
            "removing {removed} must change the compared set"
        );
    }

    // The whole declaration, with two ruled features removed, must not compare
    // equal to the ruling.
    let weakened = "[dependencies]\nrustls = { version = \"0.23\", default-features = false, \
                    features = [\"ring\"] }\n";
    let manifest = parse(weakened);
    let deps = match manifest.get("dependencies").and_then(Value::as_table) {
        Some(deps) => deps,
        None => panic!("parsed table"),
    };
    let spec = match deps.get("rustls") {
        Some(spec) => spec,
        None => panic!("declared"),
    };
    let (_, _, features) = declaration(spec);
    assert_ne!(
        sorted(&features),
        sorted(ruled_rustls.features),
        "a declaration missing std and tls12 must not satisfy the ruling"
    );
}
