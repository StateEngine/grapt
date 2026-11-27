use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use toml::Value;

const EXPECTED_EXTERNALS: &[&str] = &["serde", "serde_json", "sha2", "thiserror"];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
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

fn workspace_member_dirs(
    root: &Path,
) -> Result<BTreeMap<String, PathBuf>, Box<dyn std::error::Error>> {
    let workspace: Value = toml::from_str(&std::fs::read_to_string(root.join("Cargo.toml"))?)?;
    let mut members = BTreeMap::new();
    let Some(entries) = workspace
        .get("workspace")
        .and_then(|value| value.get("members"))
        .and_then(Value::as_array)
    else {
        return Ok(members);
    };
    for entry in entries {
        let Some(rel) = entry.as_str() else {
            continue;
        };
        let member_dir = root.join(rel);
        let manifest_path = member_dir.join("Cargo.toml");
        if let Ok(text) = std::fs::read_to_string(&manifest_path) {
            if let Ok(parsed) = text.parse::<Value>() {
                if let Some(name) = parsed
                    .get("package")
                    .and_then(|pkg| pkg.get("name"))
                    .and_then(Value::as_str)
                {
                    members.insert(name.to_owned(), normalize_path(&member_dir));
                }
            }
        }
    }
    Ok(members)
}

fn external_dependencies(
    manifest: &Value,
    member_dir: &Path,
    workspace_members: &BTreeMap<String, PathBuf>,
) -> Result<BTreeSet<String>, String> {
    if manifest
        .get("build-dependencies")
        .and_then(Value::as_table)
        .is_some_and(|table| !table.is_empty())
    {
        return Err("build-dependencies are not admitted".to_owned());
    }
    let dependencies = manifest
        .get("dependencies")
        .and_then(Value::as_table)
        .ok_or("dependencies table is absent")?;
    let mut externals = BTreeSet::new();
    for (key, spec) in dependencies {
        let name = spec
            .as_table()
            .and_then(|table| table.get("package"))
            .and_then(Value::as_str)
            .unwrap_or(key.as_str());
        let is_workspace_path = workspace_members.get(name).is_some_and(|expected_dir| {
            spec.as_table().is_some_and(|table| {
                table
                    .get("path")
                    .and_then(Value::as_str)
                    .is_some_and(|raw| {
                        !table.contains_key("version")
                            && !table.contains_key("git")
                            && !table.contains_key("registry")
                            && normalize_path(&member_dir.join(raw)) == *expected_dir
                    })
            })
        });
        if !is_workspace_path {
            externals.insert(name.to_owned());
        }
    }
    Ok(externals)
}

#[test]
fn checkpoint_crate_depends_only_on_admitted_external_libraries(
) -> Result<(), Box<dyn std::error::Error>> {
    // Workspace member edges are governed by `crates/grapt-types/tests/crate-graph.json`.
    let member_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest: Value = toml::from_str(&std::fs::read_to_string(member_dir.join("Cargo.toml"))?)?;
    let members = workspace_member_dirs(&workspace_root())?;
    let actual = external_dependencies(&manifest, &member_dir, &members)?;
    let expected = EXPECTED_EXTERNALS
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn non_workspace_grapt_aliases_and_build_deps_are_rejected(
) -> Result<(), Box<dyn std::error::Error>> {
    let member_dir = PathBuf::from("/workspace/crates/grapt-checkpoint");
    let members = BTreeMap::from([(
        "grapt-types".to_owned(),
        PathBuf::from("/workspace/crates/grapt-types"),
    )]);
    let expected = EXPECTED_EXTERNALS
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<BTreeSet<_>>();

    let aliased: Value = toml::from_str(
        "[dependencies]\nserde='1'\nserde_json='1'\nsha2='0.10'\nthiserror='2'\n\
         grapt-alias={package='reqwest',version='1'}\n",
    )?;
    assert_ne!(
        external_dependencies(&aliased, &member_dir, &members)?,
        expected
    );

    let non_workspace: Value = toml::from_str(
        "[dependencies]\nserde='1'\nserde_json='1'\nsha2='0.10'\nthiserror='2'\n\
         grapt-evil='1.0'\n",
    )?;
    assert_ne!(
        external_dependencies(&non_workspace, &member_dir, &members)?,
        expected
    );

    let out_of_workspace_same_name: Value = toml::from_str(
        "[dependencies]\nserde='1'\nserde_json='1'\nsha2='0.10'\nthiserror='2'\n\
         grapt-types={path='../../excluded/grapt-types'}\n",
    )?;
    assert!(
        external_dependencies(&out_of_workspace_same_name, &member_dir, &members)?
            .contains("grapt-types")
    );

    let build_dep: Value = toml::from_str(
        "[dependencies]\nserde='1'\nserde_json='1'\nsha2='0.10'\nthiserror='2'\n\
         [build-dependencies]\ngrapt-types={path='../grapt-types'}\n",
    )?;
    assert!(external_dependencies(&build_dep, &member_dir, &members).is_err());
    Ok(())
}

#[test]
fn sealed_type_boundary_calls_have_one_production_owner() -> Result<(), Box<dyn std::error::Error>>
{
    let roots = [
        workspace_root().join("crates/grapt-checkpoint/src"),
        workspace_root().join("crates/grapt-product/src"),
        workspace_root().join("crates/grapt-runtime/src"),
        workspace_root().join("crates/grapt-api/src"),
    ];
    let files = rust_files(&roots, 4_096)?;
    let expected = [
        "SettledRestorePlan::settle_at_provider_boundary(",
        "CheckpointRestoreAuthorityLease::issue_at_boundary(",
        "PlanPreconditionObservation::observe_at_provider_boundary(",
        "RestoreRescue::captured_at_provider_boundary(",
        "self.provider.apply_restore(admission)",
    ];
    for needle in expected {
        let occurrences = files
            .iter()
            .map(std::fs::read_to_string)
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .map(|source| source.matches(needle).count())
            .sum::<usize>();
        assert_eq!(
            occurrences, 1,
            "{needle} must have exactly one production owner"
        );
    }
    Ok(())
}

fn rust_files(
    roots: &[PathBuf],
    maximum: usize,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut pending = roots.to_vec();
    let mut files = Vec::new();
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(&path)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().and_then(|name| name.to_str()) != Some("tests") {
                    pending.push(path);
                }
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                files.push(path);
                if files.len() > maximum {
                    return Err("Rust source inventory exceeded its bound".into());
                }
            }
        }
    }
    files.sort();
    Ok(files)
}
