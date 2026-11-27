use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const EXPECTED_EXTERNALS: &[&str] = &["serde", "serde_json", "serde_path_to_error"];

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

fn metadata_for_workspace() -> Value {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(&root)
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--offline",
        ])
        .output()
        .unwrap_or_else(|error| panic!("cargo metadata failed: {error}"));
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("cargo metadata JSON: {error}"))
}

fn external_normal_deps(metadata: &Value, package_name: &str) -> Result<BTreeSet<String>, String> {
    let packages = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata missing packages")?;
    let workspace_member_ids: Option<BTreeSet<&str>> = metadata
        .get("workspace_members")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .collect::<BTreeSet<_>>()
        });
    let mut member_dirs: BTreeMap<&str, PathBuf> = BTreeMap::new();
    for pkg in packages {
        if let Some(ref member_ids) = workspace_member_ids {
            let Some(id) = pkg["id"].as_str() else {
                continue;
            };
            if !member_ids.contains(id) {
                continue;
            }
        }
        if let (Some(name), Some(manifest)) = (pkg["name"].as_str(), pkg["manifest_path"].as_str())
        {
            if let Some(dir) = Path::new(manifest).parent() {
                member_dirs.insert(name, normalize_path(dir));
            }
        }
    }
    let package = packages
        .iter()
        .find(|pkg| pkg["name"].as_str() == Some(package_name))
        .ok_or_else(|| format!("package {package_name} not found in metadata"))?;
    let deps = package["dependencies"]
        .as_array()
        .ok_or("package dependencies missing")?;
    let mut externals = BTreeSet::new();
    for dep in deps {
        let name = dep["name"].as_str().ok_or("dependency name missing")?;
        match dep["kind"].as_str() {
            Some("dev") => continue,
            Some("build") => return Err(format!("unadmitted build-dependency {name}")),
            None => {}
            Some(other) => return Err(format!("unknown dependency kind {other}")),
        }
        let is_workspace_member = dep["source"].is_null()
            && dep["path"].as_str().is_some_and(|dep_path| {
                member_dirs
                    .get(name)
                    .is_some_and(|member_dir| normalize_path(Path::new(dep_path)) == *member_dir)
            });
        if !is_workspace_member {
            externals.insert(name.to_owned());
        }
    }
    Ok(externals)
}

/// The crate's external dependency set is an exact list, resolved by Cargo.
/// Workspace member edges are governed centrally by `crate-graph.json`.
#[test]
fn schema_has_no_upward_or_runtime_dependencies() {
    let metadata = metadata_for_workspace();
    let actual = external_normal_deps(&metadata, "grapt-schema")
        .unwrap_or_else(|error| panic!("schema dependency gate: {error}"));
    let expected: BTreeSet<String> = EXPECTED_EXTERNALS
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn build_edges_non_workspace_grapt_and_grapt_aliases_are_rejected() {
    let base_pkg = |deps: Value| {
        serde_json::json!({
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
                    "dependencies": deps
                }
            ]
        })
    };
    let build_edge = base_pkg(serde_json::json!([
        {"name": "grapt-types", "kind": "build", "source": null, "path": "/workspace/crates/grapt-types"}
    ]));
    assert!(external_normal_deps(&build_edge, "grapt-schema").is_err());

    let non_workspace_grapt = base_pkg(serde_json::json!([
        {"name": "grapt-evil", "kind": null, "source": "registry+https://example.invalid"}
    ]));
    assert!(external_normal_deps(&non_workspace_grapt, "grapt-schema")
        .is_ok_and(|deps| deps.contains("grapt-evil")));

    let out_of_workspace_same_name = base_pkg(serde_json::json!([
        {"name": "grapt-types", "kind": null, "source": null, "path": "/workspace/excluded/grapt-types"}
    ]));
    assert!(
        external_normal_deps(&out_of_workspace_same_name, "grapt-schema")
            .is_ok_and(|deps| deps.contains("grapt-types"))
    );

    let aliased_external = base_pkg(serde_json::json!([
        {"name": "reqwest", "rename": "grapt-alias", "kind": null, "source": "registry+https://example.invalid"}
    ]));
    assert!(external_normal_deps(&aliased_external, "grapt-schema")
        .is_ok_and(|deps| deps.contains("reqwest")));
}
