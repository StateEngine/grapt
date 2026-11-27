use super::*;

fn scoped(
    root: &Path,
    tool: &str,
    path: &str,
    override_ignored: bool,
) -> Result<serde_json::Value, String> {
    let mut arguments = serde_json::json!({"path":path,"include_ignored":override_ignored});
    if tool == GREP_TOOL {
        arguments["pattern"] = serde_json::json!("needle");
        arguments["glob"] = serde_json::json!("**/*.rs");
    } else if tool == GLOB_TOOL {
        arguments["pattern"] = serde_json::json!("**/*.rs");
    }
    call(root, tool, arguments)
}

#[test]
fn nt_scopes_and_ancestors_preserve_ignore_and_control_policy() -> Result<(), String> {
    let scratch = Scratch::new("nt-scope-policy")?;
    let root = &scratch.0;
    std::fs::write(root.join(".gitignore"), b"ignored/\n").map_err(|error| error.to_string())?;
    std::fs::write(root.join("visible.rs"), b"needle\n").map_err(|error| error.to_string())?;
    for directory in [
        "ignored",
        "target",
        ".hidden",
        ".git",
        ".stateengine",
        ".codex",
    ] {
        std::fs::create_dir_all(root.join(directory).join("deeper"))
            .map_err(|error| error.to_string())?;
        std::fs::write(root.join(directory).join("child.rs"), b"needle\n")
            .map_err(|error| error.to_string())?;
        std::fs::write(root.join(directory).join("deeper/child.rs"), b"needle\n")
            .map_err(|error| error.to_string())?;
        for tool in [GREP_TOOL, GLOB_TOOL, LIST_TOOL] {
            let key = if tool == GREP_TOOL {
                "matches"
            } else if tool == GLOB_TOOL {
                "paths"
            } else {
                "entries"
            };
            for path in [directory.to_owned(), format!("{directory}/deeper")] {
                let named = scoped(root, tool, &path, false)?;
                if discovery::excluded(directory) {
                    assert_eq!(named[key], serde_json::json!([]));
                    assert_eq!(named["skipped"][0]["reason"], "protected-control-scope");
                } else {
                    assert!(!named[key].as_array().ok_or("rows absent")?.is_empty());
                    assert_eq!(named["complete"], true);
                }
                let overridden = scoped(root, tool, &path, true)?;
                if discovery::excluded(directory) {
                    assert_eq!(
                        overridden[key],
                        serde_json::json!([]),
                        "control scope override leaked: {tool} {path}"
                    );
                    assert_eq!(
                        overridden["skipped"][0]["reason"],
                        "protected-control-scope"
                    );
                } else {
                    assert!(!overridden[key].as_array().ok_or("rows absent")?.is_empty());
                    assert_eq!(overridden["complete"], true);
                }
            }
            let baseline = scoped(root, tool, ".", false)?;
            assert_eq!(baseline[key].as_array().ok_or("root rows absent")?.len(), 1);
        }
    }
    Ok(())
}

#[test]
fn nt_scoped_nested_negation_and_ordinary_descendants_remain_usable() -> Result<(), String> {
    let scratch = Scratch::new("nt-scope-negation")?;
    let root = &scratch.0;
    std::fs::create_dir_all(root.join("src/hidden/ordinary")).map_err(|error| error.to_string())?;
    std::fs::write(
        root.join("src/.gitignore"),
        b"hidden/\n!hidden/\nhidden/*.rs\n!hidden/keep.rs\n",
    )
    .map_err(|error| error.to_string())?;
    for name in ["keep.rs", "drop.rs", "ordinary/leaf.rs"] {
        std::fs::write(root.join("src/hidden").join(name), b"needle\n")
            .map_err(|error| error.to_string())?;
    }
    for tool in [GREP_TOOL, GLOB_TOOL, LIST_TOOL] {
        let visible = scoped(root, tool, "src/hidden", false)?;
        let text = visible.to_string();
        assert!(
            text.contains("keep.rs") && !text.contains("drop.rs"),
            "{text}"
        );
        let descendant = scoped(root, tool, "src/hidden/ordinary", false)?;
        assert!(descendant.to_string().contains("leaf.rs"));
    }
    Ok(())
}
