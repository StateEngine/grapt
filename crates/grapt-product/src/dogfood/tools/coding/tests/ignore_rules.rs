use super::*;

fn put(root: &Path, path: &str, content: impl AsRef<[u8]>) -> Result<(), String> {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().ok_or("fixture has no parent")?)
        .map_err(|error| error.to_string())?;
    std::fs::write(path, content).map_err(|error| error.to_string())
}

fn paths(root: &Path, tool: &str, args: serde_json::Value) -> Result<Vec<String>, String> {
    let found = call(root, tool, args)?;
    let key = if tool == GLOB_TOOL {
        "paths"
    } else if tool == GREP_TOOL {
        "matches"
    } else {
        "entries"
    };
    found[key]
        .as_array()
        .ok_or("missing rows")?
        .iter()
        .map(|row| {
            let path = if tool == GLOB_TOOL { row } else { &row["path"] };
            path.as_str()
                .map(str::to_owned)
                .ok_or("missing path".into())
        })
        .collect()
}

#[test]
fn p7_ignore_sources_prioritize_kind_then_depth_and_allow_opt_in() -> Result<(), String> {
    let scratch = Scratch::new("p7-ignore-precedence")?;
    let root = &scratch.0;
    put(root, ".git/info/exclude", "*.exclude\n*.git\n*.local\n")?;
    put(root, ".gitignore", "!*.git\n*.local\n*.parent\n")?;
    put(
        root,
        ".ignore",
        "!*.local\n*.deeper\n*.blocked\n!*.allowed\n",
    )?;
    put(
        root,
        "nested/.gitignore",
        "*.allowed\n!*.blocked\n!*.parent\n",
    )?;
    put(root, "nested/.ignore", "!*.deeper\n")?;
    for path in [
        "a.exclude",
        "b.git",
        "c.local",
        "nested/a.allowed",
        "nested/b.blocked",
        "nested/c.parent",
        "nested/d.deeper",
    ] {
        put(root, path, "needle\n")?;
    }
    let expected = vec![
        "b.git",
        "c.local",
        "nested/a.allowed",
        "nested/c.parent",
        "nested/d.deeper",
    ];
    assert_eq!(
        paths(root, GLOB_TOOL, serde_json::json!({"pattern":"**"}))?,
        expected
    );
    assert_eq!(
        paths(root, GREP_TOOL, serde_json::json!({"pattern":"needle"}))?,
        expected
    );
    assert_eq!(
        paths(root, LIST_TOOL, serde_json::json!({}))?,
        vec!["b.git", "c.local", "nested"]
    );
    let all = paths(
        root,
        GREP_TOOL,
        serde_json::json!({"pattern":"needle", "include_ignored":true}),
    )?;
    assert_eq!(all.len(), 7);
    assert!(!all.iter().any(|path| path.starts_with(".git/")));
    Ok(())
}

#[test]
fn p7_build_directories_require_named_scope_even_with_ignore_opt_in() -> Result<(), String> {
    let scratch = Scratch::new("p7-build-scopes")?;
    let root = &scratch.0;
    put(root, ".gitignore", "ignored/\n")?;
    put(root, "ignored/hidden.rs", "needle\n")?;
    put(root, "source.rs", "needle\n")?;
    for directory in [
        "target",
        "node_modules",
        "dist",
        "build",
        ".next",
        ".nuxt",
        "__pycache__",
        ".venv",
        "venv",
    ] {
        put(root, &format!("{directory}/named.rs"), "needle\n")?;
        for tool in [GLOB_TOOL, GREP_TOOL, LIST_TOOL] {
            for include in [false, true] {
                let mut args = serde_json::json!({"path":directory, "include_ignored":include});
                if tool != LIST_TOOL {
                    args["pattern"] = if tool == GLOB_TOOL { "**" } else { "needle" }.into();
                }
                assert_eq!(
                    paths(root, tool, args)?,
                    vec![format!("{directory}/named.rs")]
                );
            }
        }
    }
    for include in [false, true] {
        let expected = if include {
            vec!["ignored/hidden.rs", "source.rs"]
        } else {
            vec!["source.rs"]
        };
        assert_eq!(
            paths(
                root,
                GLOB_TOOL,
                serde_json::json!({"pattern":"**/*.rs", "include_ignored":include})
            )?,
            expected
        );
        assert_eq!(
            paths(
                root,
                GREP_TOOL,
                serde_json::json!({"pattern":"needle", "include_ignored":include})
            )?,
            expected
        );
        let expected_list = if include {
            vec![".gitignore", "ignored", "source.rs"]
        } else {
            vec!["source.rs"]
        };
        assert_eq!(
            paths(
                root,
                LIST_TOOL,
                serde_json::json!({"include_ignored":include})
            )?,
            expected_list
        );
    }
    // Naming a scope opens it; ordinary ignore rules still filter its children.
    put(root, "target/.ignore", "*.rs\n")?;
    assert_eq!(
        paths(
            root,
            GLOB_TOOL,
            serde_json::json!({"path":"target", "pattern":"*.rs"})
        )?,
        Vec::<String>::new()
    );
    assert_eq!(
        paths(
            root,
            GLOB_TOOL,
            serde_json::json!({"path":"target", "pattern":"*.rs", "include_ignored":true})
        )?,
        vec!["target/named.rs"]
    );
    Ok(())
}

#[test]
fn p7_linked_worktree_reads_only_bounded_git_exclude_metadata() -> Result<(), String> {
    let scratch = Scratch::new("p7-worktree")?;
    let metadata = Scratch::new("p7-git-metadata")?;
    let root = &scratch.0;
    put(&metadata.0, "worktrees/seat/commondir", "../..\n")?;
    put(&metadata.0, "info/exclude", "*.private\n")?;
    put(root, "hidden.private", "needle\n")?;
    put(root, "visible.rs", "needle\n")?;
    let directory = metadata
        .0
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("metadata name")?;
    for target in [
        format!("../{directory}/worktrees/seat"),
        metadata
            .0
            .join("worktrees/seat")
            .to_string_lossy()
            .into_owned(),
    ] {
        put(root, ".git", format!("gitdir: {target}\n"))?;
        assert_eq!(
            paths(root, GLOB_TOOL, serde_json::json!({"pattern":"**"}))?,
            vec!["visible.rs"]
        );
        assert_eq!(
            paths(
                root,
                GREP_TOOL,
                serde_json::json!({"pattern":"needle", "include_ignored":true})
            )?,
            vec!["hidden.private", "visible.rs"]
        );
    }
    put(&metadata.0, "info/exclude", vec![b'x'; 64 * 1024 + 1])?;
    let error = call(root, GLOB_TOOL, serde_json::json!({"pattern":"**"}))
        .err()
        .ok_or("oversized metadata accepted")?;
    assert!(error.contains("byte bound"), "{error}");
    Ok(())
}

#[test]
fn p7_ignore_metadata_bounds_and_diagnostics_do_not_quote_patterns() -> Result<(), String> {
    let scratch = Scratch::new("p7-ignore-bounds")?;
    let root = &scratch.0;
    for (path, data, expected) in [
        (".ignore", vec![0xff], "not UTF-8"),
        (".ignore", vec![b'\n'; 4097], "line bound"),
        (".ignore", vec![b'x'; 64 * 1024 + 1], "byte bound"),
        (
            ".ignore",
            b"[z-a]private-secret\n".to_vec(),
            "pattern invalid",
        ),
    ] {
        put(root, path, data)?;
        let error = call(root, GLOB_TOOL, serde_json::json!({"pattern":"**"}))
            .err()
            .ok_or_else(|| format!("invalid metadata accepted: {expected}"))?;
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains("private-secret"), "{error}");
    }
    std::fs::remove_file(root.join(".ignore")).map_err(|error| error.to_string())?;
    put(root, ".git", "gitdir: ../missing\nextra\n")?;
    assert!(call(root, GLOB_TOOL, serde_json::json!({"pattern":"**"})).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn p7_ignore_metadata_refuses_linked_files_and_git_parents() -> Result<(), String> {
    let scratch = Scratch::new("p7-ignore-links")?;
    let outside = Scratch::new("p7-ignore-links-outside")?;
    let root = &scratch.0;
    put(&outside.0, "info/exclude", "private-secret\n")?;
    std::os::unix::fs::symlink(outside.0.join("info/exclude"), root.join(".ignore"))
        .map_err(|error| error.to_string())?;
    assert!(call(root, GLOB_TOOL, serde_json::json!({"pattern":"**"})).is_err());
    std::fs::remove_file(root.join(".ignore")).map_err(|error| error.to_string())?;
    std::os::unix::fs::symlink(&outside.0, root.join("linked"))
        .map_err(|error| error.to_string())?;
    put(root, ".git", "gitdir: linked\n")?;
    assert!(call(root, GLOB_TOOL, serde_json::json!({"pattern":"**"})).is_err());
    Ok(())
}
