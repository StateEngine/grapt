use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use toml::Value;

// External production dependencies only; workspace edges live in `crate-graph.json`.
const PRODUCTION: &[&str] = &["rquickjs", "serde", "serde_json", "thiserror"];

fn normalize_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| {
        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    normalized.pop();
                }
                other => normalized.push(other.as_os_str()),
            }
        }
        normalized
    })
}

fn workspace_member_dirs(root: &Path, workspace: &Value) -> BTreeMap<String, PathBuf> {
    let mut members = BTreeMap::new();
    let Some(entries) = workspace
        .get("workspace")
        .and_then(|value| value.get("members"))
        .and_then(Value::as_array)
    else {
        return members;
    };
    for entry in entries {
        let Some(rel) = entry.as_str() else {
            continue;
        };
        let member_dir = root.join(rel);
        let manifest_path = member_dir.join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(parsed) = text.parse::<Value>() else {
            continue;
        };
        if let Some(name) = parsed
            .get("package")
            .and_then(|pkg| pkg.get("name"))
            .and_then(Value::as_str)
        {
            members.insert(name.to_owned(), normalize_path(&member_dir));
        }
    }
    members
}

fn is_workspace_member_dep(
    name: &str,
    spec: &Value,
    base_dir: &Path,
    workspace_members: &BTreeMap<String, PathBuf>,
) -> bool {
    let Some(expected_dir) = workspace_members.get(name) else {
        return false;
    };
    let Some(table) = spec.as_table() else {
        return false;
    };
    let Some(raw_path) = table.get("path").and_then(Value::as_str) else {
        return false;
    };
    !table.contains_key("version")
        && !table.contains_key("git")
        && !table.contains_key("registry")
        && normalize_path(&base_dir.join(raw_path)) == *expected_dir
}

fn resolved(key: &str, value: &Value, root: &Value) -> Result<(String, Value, bool), String> {
    let mut value = value.clone();
    let from_workspace = value.get("workspace").and_then(Value::as_bool) == Some(true);
    if from_workspace {
        let inherited = root
            .get("workspace")
            .and_then(|v| v.get("dependencies"))
            .and_then(|v| v.get(key))
            .ok_or("missing workspace dependency")?;
        let mut base = inherited
            .as_table()
            .cloned()
            .ok_or("workspace dependency must be a table")?;
        let local = value.as_table().ok_or("dependency must be a table")?;
        for (field, value) in local {
            if field == "workspace" {
                continue;
            }
            if field != "features" {
                return Err("workspace override is not admitted".into());
            }
            let mut features = base
                .get("features")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            features.extend(
                value
                    .as_array()
                    .ok_or("malformed inherited features")?
                    .clone(),
            );
            base.insert("features".into(), Value::Array(features));
        }
        value = Value::Table(base);
    }
    let name = match value.get("package") {
        Some(Value::String(name)) => name.clone(),
        Some(_) => return Err("malformed package alias".into()),
        None => key.to_owned(),
    };
    Ok((name, value, from_workspace))
}

fn runtime_features(value: &Value) -> Result<(), String> {
    if value.get("version").and_then(Value::as_str) != Some("=0.13.0")
        || value.get("default-features").and_then(Value::as_bool) != Some(false)
    {
        return Err("QuickJS version/default features changed".into());
    }
    let features = value
        .get("features")
        .and_then(Value::as_array)
        .ok_or("missing runtime features")?;
    if features.len() != 1 || features[0].as_str() != Some("std") {
        return Err("QuickJS must remain std-only".into());
    }
    Ok(())
}

fn inspect(
    value: &Value,
    root: &Value,
    member_dir: &Path,
    workspace_root: &Path,
    workspace_members: &BTreeMap<String, PathBuf>,
    production: &mut BTreeSet<String>,
) -> Result<(), String> {
    let table = value.as_table().ok_or("manifest table missing")?;
    for (scope, entries) in table {
        if matches!(
            scope.as_str(),
            "dependencies" | "dev-dependencies" | "build-dependencies"
        ) {
            for (key, spec) in entries.as_table().ok_or("dependency table malformed")? {
                let (name, spec, from_workspace) = resolved(key, spec, root)?;
                let base_dir = if from_workspace {
                    workspace_root
                } else {
                    member_dir
                };
                match scope.as_str() {
                    // Normal workspace member edges are governed by `crate-graph.json`.
                    // Build-dependencies are NEVER exempted, nor are non-workspace `grapt-*` crates.
                    "dependencies"
                        if is_workspace_member_dep(&name, &spec, base_dir, workspace_members) => {}
                    "dependencies" if PRODUCTION.contains(&name.as_str()) => {
                        if name == "rquickjs" {
                            runtime_features(&spec)?;
                        }
                        production.insert(name);
                    }
                    "dev-dependencies" if name == "toml" => {}
                    _ => return Err(format!("unadmitted {scope} edge {name}")),
                }
            }
        } else if scope == "target" {
            for target in entries.as_table().ok_or("target table malformed")?.values() {
                inspect(
                    target,
                    root,
                    member_dir,
                    workspace_root,
                    workspace_members,
                    production,
                )?;
            }
        } else if scope == "features"
            && entries
                .as_table()
                .is_none_or(|features| !features.is_empty())
        {
            return Err("feature forwarding is not admitted".into());
        }
    }
    Ok(())
}

fn parse(text: &str) -> Value {
    text.parse()
        .unwrap_or_else(|error| panic!("manifest: {error}"))
}

#[test]
fn exact_production_edges_and_locked_runtime_family() {
    let member = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = member
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| panic!("workspace root missing"));
    let manifest = parse(
        &std::fs::read_to_string(member.join("Cargo.toml"))
            .unwrap_or_else(|error| panic!("member: {error}")),
    );
    let workspace = parse(
        &std::fs::read_to_string(root.join("Cargo.toml"))
            .unwrap_or_else(|error| panic!("workspace: {error}")),
    );
    let members = workspace_member_dirs(root, &workspace);
    let mut production = BTreeSet::new();
    inspect(
        &manifest,
        &workspace,
        member,
        root,
        &members,
        &mut production,
    )
    .unwrap_or_else(|error| panic!("dependency gate: {error}"));
    assert_eq!(
        production,
        PRODUCTION.iter().map(|name| (*name).to_owned()).collect()
    );
    let lock = parse(
        &std::fs::read_to_string(root.join("Cargo.lock"))
            .unwrap_or_else(|error| panic!("lock: {error}")),
    );
    let packages = lock
        .get("package")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("lock packages missing"));
    for name in ["rquickjs", "rquickjs-core", "rquickjs-sys"] {
        let versions: Vec<_> = packages
            .iter()
            .filter(|p| p.get("name").and_then(Value::as_str) == Some(name))
            .map(|p| p.get("version").and_then(Value::as_str))
            .collect();
        assert_eq!(versions, vec![Some("0.13.0")], "runtime family pin {name}");
    }
}

#[test]
fn aliases_target_tables_build_edges_and_non_workspace_grapt_cannot_evade_gate() {
    let root = parse("[workspace.dependencies]\njs={package='rquickjs',version='=0.13.0',default-features=false,features=['std']}\n");
    let workspace_root = Path::new("/workspace");
    let member_dir = Path::new("/workspace/crates/grapt-codemode");
    let members = BTreeMap::from([(
        "grapt-types".to_owned(),
        PathBuf::from("/workspace/crates/grapt-types"),
    )]);
    for text in [
        "[dependencies]\nx={package='reqwest',version='1'}",
        "[dependencies]\ngrapt-alias={package='reqwest',version='1'}",
        "[dependencies]\ngrapt-evil='1.0'",
        "[dependencies]\ngrapt-types='1.0'",
        "[dependencies]\ngrapt-types={path='../../excluded/grapt-types'}",
        "[build-dependencies]\ngrapt-types={path='../grapt-types'}",
        "[target.'cfg(windows)'.dependencies]\nx={package='tokio',version='1'}",
        "[dependencies]\njs={workspace=true,features=['loader']}",
        "[dependencies]\nx={package='rquickjs',version='=0.13.0',features=['std']}",
        "[features]\nescape=['rquickjs/allocator']",
    ] {
        assert!(
            inspect(
                &parse(text),
                &root,
                member_dir,
                workspace_root,
                &members,
                &mut BTreeSet::new()
            )
            .is_err(),
            "mutant survived: {text}"
        );
    }
    let valid = parse("[dependencies]\njs={workspace=true}\ngrapt-types={path='../grapt-types'}");
    let mut names = BTreeSet::new();
    inspect(
        &valid,
        &root,
        member_dir,
        workspace_root,
        &members,
        &mut names,
    )
    .unwrap_or_else(|error| panic!("valid alias: {error}"));
    assert!(names.contains("rquickjs"));
}
