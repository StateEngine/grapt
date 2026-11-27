//! Admits safe Windows ConPTY APIs only. Keep exact versions, the blocking feature, and
//! the Windows-only boundary.
use toml::Value;

fn parse(text: &str) -> Value {
    text.parse()
        .unwrap_or_else(|error| panic!("invalid dependency manifest: {error}"))
}

fn manifest() -> Value {
    parse(include_str!("../Cargo.toml"))
}

fn admitted_edges(value: &Value) -> Vec<(String, String, Value)> {
    let mut edges = Vec::new();
    let mut collect = |scope: &str, table: &Value| {
        for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
            if let Some(dependencies) = table.get(kind).and_then(Value::as_table) {
                for (alias, spec) in dependencies {
                    let name = spec.get("package").and_then(Value::as_str).unwrap_or(alias);
                    if matches!(name, "conpty-oxide" | "windows-spawn") {
                        edges.push((scope.to_owned(), name.to_owned(), spec.clone()));
                    }
                }
            }
        }
    };
    collect("unconditional", value);
    if let Some(targets) = value.get("target").and_then(Value::as_table) {
        for (scope, target) in targets {
            collect(scope, target);
        }
    }
    edges
}

#[test]
fn terminal_ffi_dependencies_are_exact_and_windows_only() {
    let edges = admitted_edges(&manifest());
    assert_eq!(
        edges.len(),
        2,
        "exactly the two admitted Windows edges must exist"
    );
    for (name, version, features) in [
        ("conpty-oxide", "=0.1.2", vec!["blocking"]),
        ("windows-spawn", "=0.1.0", vec![]),
    ] {
        let matches: Vec<_> = edges.iter().filter(|(_, found, _)| found == name).collect();
        assert_eq!(matches.len(), 1, "{name} must have one declaration");
        let (scope, _, entry) = matches[0];
        assert_eq!(scope, "cfg(windows)", "{name} escaped the Windows boundary");
        assert_eq!(entry.get("version").and_then(Value::as_str), Some(version));
        assert_eq!(
            entry.get("default-features").and_then(Value::as_bool),
            Some(false)
        );
        let actual: Vec<_> = entry
            .get("features")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .map(|value| value.as_str().unwrap_or("INVALID"))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(actual, features, "{name} enabled an unreviewed feature");
    }
    let lock = parse(include_str!("../../../Cargo.lock"));
    let packages = lock
        .get("package")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("Cargo.lock has no package table"));
    for (name, version) in [("conpty-oxide", "0.1.2"), ("windows-spawn", "0.1.0")] {
        let resolved: Vec<_> = packages
            .iter()
            .filter(|package| package.get("name").and_then(Value::as_str) == Some(name))
            .map(|package| package.get("version").and_then(Value::as_str))
            .collect();
        assert_eq!(resolved, [Some(version)], "{name} resolution drifted");
    }
}

#[test]
fn the_attestation_observes_unconditional_target_and_alias_edges() {
    for source in [
        "[dependencies]\nconpty-oxide = '=0.1.2'",
        "[target.'cfg(unix)'.dependencies]\nconpty-oxide = '=0.1.2'",
        "[build-dependencies.alias]\npackage = 'windows-spawn'\nversion = '=0.1.0'",
    ] {
        assert_eq!(admitted_edges(&parse(source)).len(), 1);
    }
}
