//! Structural posture gate for external dependencies of the executable product root.
//!
//! Workspace `grapt-*` edges are governed centrally by
//! `crates/grapt-types/tests/crate-graph.json`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use toml::Value;

const DEP_TABLES: &[&str] = &["dependencies", "dev-dependencies", "build-dependencies"];

#[path = "dependency_direction/schema_posture.rs"]
mod schema_posture;

fn workspace_dependencies(workspace: &Value) -> Option<&toml::value::Table> {
    workspace
        .get("workspace")
        .and_then(Value::as_table)
        .and_then(|table| table.get("dependencies"))
        .and_then(Value::as_table)
}

fn package_or_key(key: &str, table: &toml::value::Table) -> Result<String, String> {
    match table.get("package") {
        None => Ok(key.to_owned()),
        Some(Value::String(package)) if !package.is_empty() => Ok(package.clone()),
        Some(other) => Err(format!(
            "malformed `package` rename: expected a non-empty string, found {other:?}"
        )),
    }
}

fn root_definition_name(key: &str, definition: &Value) -> Result<String, String> {
    match definition {
        Value::String(version) if !version.is_empty() => Ok(key.to_owned()),
        Value::String(_) => {
            Err("malformed workspace definition: empty version requirement".to_owned())
        }
        Value::Table(table) => {
            if table.contains_key("workspace") {
                return Err(
                    "malformed workspace definition: a root definition cannot carry `workspace`"
                        .to_owned(),
                );
            }
            package_or_key(key, table)
        }
        other => Err(format!(
            "malformed workspace definition: expected a version string or a table, found {other:?}"
        )),
    }
}

fn effective_name(
    key: &str,
    spec: &Value,
    workspace: Option<&toml::value::Table>,
) -> Result<String, String> {
    let table = match spec {
        Value::Table(table) => table,
        Value::String(version) if !version.is_empty() => return Ok(key.to_owned()),
        Value::String(_) => {
            return Err("malformed dependency: empty version requirement".to_owned());
        }
        other => {
            return Err(format!(
                "malformed dependency: expected a version string or a table, found {other:?}"
            ));
        }
    };

    match table.get("workspace") {
        None => package_or_key(key, table),
        Some(Value::Boolean(true)) => {
            if table.contains_key("package") {
                return Err(
                    "ambiguous: `workspace = true` cannot be combined with `package`".to_owned(),
                );
            }
            let Some(workspace) = workspace else {
                return Err(
                    "inherits from the workspace, which declares no [workspace.dependencies]"
                        .to_owned(),
                );
            };
            let Some(inherited) = workspace.get(key) else {
                return Err(format!(
                    "inherits `{key}`, which [workspace.dependencies] does not define"
                ));
            };
            root_definition_name(key, inherited)
        }
        Some(other) => Err(format!(
            "malformed `workspace` key: expected the boolean true, found {other:?}"
        )),
    }
}

fn parse(source: &str) -> Value {
    match source.parse::<Value>() {
        Ok(value) => value,
        Err(error) => panic!("manifest parse: {error}"),
    }
}

fn workspace_manifest(start: &Path) -> (PathBuf, Value) {
    for directory in start.ancestors() {
        let candidate = directory.join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&candidate) else {
            continue;
        };
        let value = parse(&text);
        if value.get("workspace").and_then(Value::as_table).is_some() {
            return (candidate, value);
        }
    }
    panic!(
        "no ancestor of {} declares a [workspace] table",
        start.display()
    );
}
