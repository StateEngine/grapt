//! Workspace law: every crate's workspace dependencies are an exact, reviewed list.
//!
//! Cargo itself resolves the graph (`cargo metadata --no-deps`), so renamed,
//! workspace-inherited and target-specific dependencies all appear under their
//! real package names; no manifest spelling can hide an edge. Adding a crate or
//! an edge is an edit to `crate-graph.json` that review sees, which keeps lower
//! crates from reaching upward. Every member must also inherit the workspace
//! lint set, which carries the structure limits.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const GRAPH: &str = "tests/crate-graph.json";

#[derive(Debug, Default, PartialEq, Eq)]
struct Edges {
    deps: BTreeSet<String>,
    build: BTreeSet<String>,
    dev: BTreeSet<String>,
}

type Graph = BTreeMap<String, Edges>;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn metadata(root: &Path) -> Result<Value, Box<dyn Error>> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(root)
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--offline",
        ])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn packages(metadata: &Value) -> Result<&Vec<Value>, String> {
    metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())
}

fn text<'a>(value: &'a Value, label: &str) -> Result<&'a str, String> {
    value
        .as_str()
        .ok_or_else(|| format!("{label} is not a string"))
}

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

/// The resolved graph, restricted to workspace members.
fn actual(metadata: &Value) -> Result<Graph, String> {
    let packages = packages(metadata)?;
    let workspace_member_ids: Option<BTreeSet<&str>> = metadata
        .get("workspace_members")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .map(|id| text(id, "workspace_members entry"))
                .collect::<Result<BTreeSet<_>, _>>()
        })
        .transpose()?;
    let mut member_dirs: BTreeMap<&str, PathBuf> = BTreeMap::new();
    for package in packages {
        if let Some(ref member_ids) = workspace_member_ids {
            let id = text(&package["id"], "package id")?;
            if !member_ids.contains(id) {
                continue;
            }
        }
        let name = text(&package["name"], "package name")?;
        let manifest_path = Path::new(text(&package["manifest_path"], "manifest_path")?);
        let dir = manifest_path
            .parent()
            .ok_or_else(|| format!("manifest_path for {name} has no parent directory"))?;
        member_dirs.insert(name, normalize_path(dir));
    }
    let mut graph = Graph::new();
    for package in packages {
        let pkg_name = text(&package["name"], "package name")?;
        if !member_dirs.contains_key(pkg_name) {
            continue;
        }
        let edges = graph.entry(pkg_name.to_owned()).or_default();
        let dependencies = package["dependencies"]
            .as_array()
            .ok_or("package dependencies is not an array")?;
        for dependency in dependencies {
            let name = text(&dependency["name"], "dependency name")?;
            let Some(expected_dir) = member_dirs.get(name) else {
                continue;
            };
            if !dependency["source"].is_null() {
                continue;
            }
            let Some(dep_path) = dependency["path"].as_str() else {
                continue;
            };
            if normalize_path(Path::new(dep_path)) != *expected_dir {
                return Err(format!(
                    "{pkg_name}: dependency {name} at {dep_path} shares a workspace member name \
                     but points outside the workspace member directory {}",
                    expected_dir.display()
                ));
            }
            match dependency["kind"].as_str() {
                None => edges.deps.insert(name.to_owned()),
                Some("build") => edges.build.insert(name.to_owned()),
                Some("dev") => edges.dev.insert(name.to_owned()),
                Some(other) => return Err(format!("unknown dependency kind {other}")),
            };
        }
        let deps = edges.deps.clone();
        edges.dev.retain(|name| !deps.contains(name));
    }
    Ok(graph)
}

fn listed(source: &str) -> Result<Graph, String> {
    let document: Value = serde_json::from_str(source).map_err(|e| format!("{GRAPH}: {e}"))?;
    let crates = document["crates"]
        .as_object()
        .ok_or_else(|| format!("{GRAPH}: crates must be an object"))?;
    let names = |value: &Value, label: &str| -> Result<BTreeSet<String>, String> {
        value
            .as_array()
            .ok_or_else(|| format!("{GRAPH}: {label} must be an array"))?
            .iter()
            .map(|name| text(name, label).map(str::to_owned))
            .collect()
    };
    crates
        .iter()
        .map(|(name, edges)| {
            let build = match edges.get("build") {
                Some(value) => names(value, &format!("{name}.build"))?,
                None => BTreeSet::new(),
            };
            Ok((
                name.clone(),
                Edges {
                    deps: names(&edges["deps"], &format!("{name}.deps"))?,
                    build,
                    dev: names(&edges["dev"], &format!("{name}.dev"))?,
                },
            ))
        })
        .collect()
}

fn differences(actual: &Graph, listed: &Graph) -> Vec<String> {
    let mut problems = Vec::new();
    for (name, now) in actual {
        let Some(allowed) = listed.get(name) else {
            problems.push(format!("{name}: new crate; add it to {GRAPH}"));
            continue;
        };
        for (kind, now, allowed) in [
            ("deps", &now.deps, &allowed.deps),
            ("build", &now.build, &allowed.build),
            ("dev", &now.dev, &allowed.dev),
        ] {
            for added in now.difference(allowed) {
                problems.push(format!(
                    "{name}: new {kind} edge to {added}; add it to {GRAPH} for review"
                ));
            }
            for gone in allowed.difference(now) {
                problems.push(format!(
                    "{name}: no longer uses {gone} ({kind}); remove it from {GRAPH}"
                ));
            }
        }
    }
    for name in listed.keys().filter(|name| !actual.contains_key(*name)) {
        problems.push(format!(
            "{name}: not a workspace member; remove it from {GRAPH}"
        ));
    }
    problems
}

#[test]
fn workspace_dependencies_match_the_reviewed_graph() -> Result<(), Box<dyn Error>> {
    let root = workspace_root();
    let source = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(GRAPH))?;
    let problems = differences(&actual(&metadata(&root)?)?, &listed(&source)?);
    if problems.is_empty() {
        return Ok(());
    }
    Err(problems.join("\n").into())
}

#[test]
fn every_member_inherits_the_workspace_lints() -> Result<(), Box<dyn Error>> {
    let metadata = metadata(&workspace_root())?;
    let mut missing = Vec::new();
    for package in packages(&metadata)? {
        let manifest = std::fs::read_to_string(text(&package["manifest_path"], "manifest_path")?)?;
        let parsed: toml::Value = toml::from_str(&manifest)?;
        let inherits = parsed
            .get("lints")
            .and_then(|lints| lints.get("workspace"))
            .and_then(toml::Value::as_bool);
        if inherits != Some(true) {
            missing.push(text(&package["name"], "package name")?.to_owned());
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!("these crates must declare `[lints] workspace = true`: {missing:?}").into())
}

#[test]
fn new_edges_and_stale_entries_are_both_reported() {
    let set = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect();
    let actual = Graph::from([
        (
            "low".to_owned(),
            Edges {
                deps: set(&["high"]),
                build: set(&["high"]),
                dev: set(&[]),
            },
        ),
        ("high".to_owned(), Edges::default()),
    ]);
    let listed = Graph::from([
        (
            "low".to_owned(),
            Edges {
                deps: set(&[]),
                build: set(&[]),
                dev: set(&["gone"]),
            },
        ),
        ("high".to_owned(), Edges::default()),
        ("removed".to_owned(), Edges::default()),
    ]);
    let problems = differences(&actual, &listed);
    assert_eq!(problems.len(), 4, "{problems:?}");
    assert!(problems[0].contains("new deps edge to high"));
    assert!(problems[1].contains("new build edge to high"));
    assert!(problems[2].contains("no longer uses gone"));
    assert!(problems[3].contains("removed: not a workspace member"));
}

#[test]
fn out_of_workspace_path_crate_sharing_member_name_is_rejected() {
    let metadata = serde_json::json!({
        "workspace_members": [
            "path+file:///workspace/crates/grapt-types#0.1.0",
            "path+file:///workspace/crates/grapt-schema#0.1.0"
        ],
        "packages": [
            {
                "id": "path+file:///workspace/crates/grapt-types#0.1.0",
                "name": "grapt-types",
                "manifest_path": "/workspace/crates/grapt-types/Cargo.toml",
                "dependencies": []
            },
            {
                "id": "path+file:///workspace/crates/grapt-schema#0.1.0",
                "name": "grapt-schema",
                "manifest_path": "/workspace/crates/grapt-schema/Cargo.toml",
                "dependencies": [
                    {
                        "name": "grapt-types",
                        "kind": null,
                        "source": null,
                        "path": "/workspace/excluded/grapt-types"
                    }
                ]
            }
        ]
    });
    assert!(actual(&metadata).is_err_and(|err| err.contains("outside the workspace member")));
}
