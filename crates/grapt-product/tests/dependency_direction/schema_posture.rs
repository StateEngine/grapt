use super::*;

fn inspect(manifest: &Value, root: &Value) -> Result<usize, String> {
    let mut count = 0;
    let mut aliases = BTreeSet::new();
    let inherited = workspace_dependencies(root);
    let mut scopes = vec![manifest];
    if let Some(targets) = manifest.get("target").and_then(Value::as_table) {
        scopes.extend(targets.values());
    }
    for scope in scopes {
        for name in DEP_TABLES {
            let Some(deps) = scope.get(*name).and_then(Value::as_table) else {
                continue;
            };
            for (key, spec) in deps {
                if effective_name(key, spec, inherited)? != "jsonschema" {
                    continue;
                }
                count += 1;
                aliases.insert(key.as_str());
                let table = spec
                    .as_table()
                    .ok_or("jsonschema posture must be explicit")?;
                let table = if table.get("workspace").and_then(Value::as_bool) == Some(true) {
                    if table.len() != 1 {
                        return Err("jsonschema member overrides refused".into());
                    }
                    inherited
                        .and_then(|deps| deps.get(key))
                        .and_then(Value::as_table)
                        .ok_or("jsonschema inheritance invalid")?
                } else {
                    table
                };
                if table.get("version").and_then(Value::as_str) != Some("=0.56.0")
                    || table.get("default-features").and_then(Value::as_bool) != Some(false)
                    || table.keys().any(|key| {
                        !["version", "default-features", "package"].contains(&key.as_str())
                    })
                {
                    return Err("jsonschema exact offline dependency posture changed".into());
                }
            }
        }
    }
    if let Some(features) = manifest.get("features").and_then(Value::as_table) {
        for feature in features.values().filter_map(Value::as_array).flatten() {
            if feature
                .as_str()
                .and_then(|name| name.split_once('/'))
                .is_some_and(|(name, _)| aliases.contains(name.trim_end_matches('?')))
            {
                return Err("jsonschema feature forwarding refused".into());
            }
        }
    }
    Ok(count)
}

#[test]
fn schema_dependency_is_product_only_exact_and_offline() -> Result<(), Box<dyn std::error::Error>> {
    let member = Path::new(env!("CARGO_MANIFEST_DIR"));
    let (root_path, root) = workspace_manifest(member);
    let product = parse(&std::fs::read_to_string(member.join("Cargo.toml"))?);
    assert!(inspect(&product, &root)? > 0);
    let directory = root_path.parent().ok_or("workspace parent")?;
    for leaf in ["grapt-codemode", "grapt-mcp"] {
        let manifest = parse(&std::fs::read_to_string(
            directory.join("crates").join(leaf).join("Cargo.toml"),
        )?);
        assert_eq!(
            inspect(&manifest, &root)?,
            0,
            "{leaf} must not acquire this dependency"
        );
    }
    Ok(())
}

#[test]
fn renamed_target_and_inherited_feature_escapes_refuse() {
    for member in [
        "[dependencies]\njsonschema='0.56'",
        "[target.'cfg(windows)'.dependencies]\nother={package='jsonschema',version='=0.56.0',default-features=false,features=['resolve-file']}",
        "[dependencies]\nother={workspace=true,features=['resolve-http']}",
    ] {
        let root = parse("[workspace.dependencies]\nother={package='jsonschema',version='=0.56.0',default-features=false}");
        assert!(inspect(&parse(member), &root).is_err());
    }
}
