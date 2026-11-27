use super::*;

mod compact_read;
mod corrections;
mod ignore_rules;
mod list_args;
mod outside_workspace;
mod profile_protection;
mod scoped_rules;
mod search_edit;
mod staging_claim;
mod staging_crash;
mod tool_errors;
#[cfg(windows)]
mod windows_paths;
mod write_overwrite;
mod write_parent;

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Result<Self, String> {
        let path =
            std::env::temp_dir().join(format!("grapt-first-use-{}-{label}", std::process::id()));
        std::fs::create_dir(&path)
            .map_err(|error| format!("exclusive scratch creation: {error}"))?;
        Ok(Self(
            std::fs::canonicalize(path).map_err(|error| error.to_string())?,
        ))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _cleanup = std::fs::remove_dir_all(&self.0);
    }
}

fn input(value: serde_json::Value) -> Result<BoundedToolInput, String> {
    BoundedToolInput::new(serde_json::to_vec(&value).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}

// Machine-oriented fixtures explicitly opt into the stable structured view.
// Compact/default regressions use call_bytes with unmodified public arguments.
fn call(
    root: &Path,
    tool: &str,
    mut value: serde_json::Value,
) -> Result<serde_json::Value, String> {
    if tool == READ_LINES_TOOL {
        value
            .as_object_mut()
            .ok_or("arguments must be an object")?
            .entry("structured")
            .or_insert(true.into());
    }
    serde_json::from_slice(&call_bytes(root, tool, value)?).map_err(|error| error.to_string())
}

fn call_bytes(root: &Path, tool: &str, value: serde_json::Value) -> Result<Vec<u8>, String> {
    let input = input(value)?;
    let prepared = prepare(root, tool, &input)?;
    let EffectTarget::Coding(target) = prepared.target else {
        return Err("wrong prepared effect".into());
    };
    run(root, &target, &input)
}

#[test]
fn fu01_discover_search_create_edit_read_exact_bytes() -> Result<(), String> {
    let scratch = Scratch::new("journey")?;
    let root = &scratch.0;
    std::fs::create_dir(root.join("src")).map_err(|error| error.to_string())?;
    std::fs::write(root.join("src/lib.rs"), "pub fn answer() -> u32 { 41 }\n")
        .map_err(|error| error.to_string())?;
    let found = call(root, GLOB_TOOL, serde_json::json!({"pattern":"**/*.rs"}))?;
    assert_eq!(found["paths"], serde_json::json!(["src/lib.rs"]));
    let scoped = call(
        root,
        GLOB_TOOL,
        serde_json::json!({"path":"src","pattern":"*.rs"}),
    )?;
    assert_eq!(
        scoped["paths"], found["paths"],
        "scoped discovery must return paths usable by read/edit"
    );
    let found = call(
        root,
        GREP_TOOL,
        serde_json::json!({"pattern":"answer", "glob":"**/*.rs"}),
    )?;
    assert_eq!(found["matches"][0]["path"], "src/lib.rs");
    assert_eq!(found["matches"][0]["line"], 1);
    call(
        root,
        WRITE_TOOL,
        serde_json::json!({"path":"src/new.rs","content":"// answer: 41\n"}),
    )?;
    let edit = input(serde_json::json!({"path":"src/new.rs","old_text":"41","new_text":"42"}))?;
    run_edit(root, &root.join("src/new.rs"), &edit)?;
    let read = input(serde_json::json!({"path":"src/new.rs"}))?;
    assert!(run_read(root, &root.join("src/new.rs"), &read)?.starts_with(b"1: // answer: 42\n"));
    for name in [WRITE_TOOL, GLOB_TOOL, GREP_TOOL] {
        assert!(definitions()
            .iter()
            .any(|definition| definition.name == name));
        assert!(BUILTIN_TOOL_NAMES.contains(&name));
    }
    Ok(())
}

#[test]
fn nt_read_lines_preserves_byte_read_and_reports_unicode_ranges() -> Result<(), String> {
    let scratch = Scratch::new("nt-lines")?;
    let root = &scratch.0;
    std::fs::write(root.join("source.rs"), "α\r\nβeta\nlast").map_err(|error| error.to_string())?;
    let view = call(
        root,
        READ_LINES_TOOL,
        serde_json::json!({
            "path":"source.rs", "start_line":2, "line_count":1
        }),
    )?;
    assert_eq!(view["totalLines"], 3);
    assert_eq!(view["nextLine"], 3);
    assert_eq!(view["complete"], false);
    assert_eq!(view["lines"][0]["line"], 2);
    assert_eq!(view["lines"][0]["text"], "βeta");
    assert_eq!(view["lines"][0]["byteStart"], 4);
    assert_eq!(view["lines"][0]["byteEnd"], 10);
    let window = input(serde_json::json!({"path":"source.rs","offset":2,"limit":1}))?;
    let read = run_read(root, &root.join("source.rs"), &window)?;
    assert!(read.starts_with("2: βeta\n".as_bytes()));
    assert!(definitions()
        .iter()
        .any(|tool| tool.name == READ_LINES_TOOL));
    assert!(BUILTIN_TOOL_NAMES.contains(&READ_LINES_TOOL));
    Ok(())
}

#[test]
fn nt_read_lines_distinguishes_invalid_and_unavailable() -> Result<(), String> {
    let scratch = Scratch::new("nt-line-failures")?;
    let root = &scratch.0;
    std::fs::write(root.join("binary"), [0xff]).map_err(|error| error.to_string())?;
    let invalid = call(root, READ_LINES_TOOL, serde_json::json!({"path":"binary"}))
        .err()
        .ok_or("invalid UTF-8 unexpectedly read")?;
    assert!(invalid.contains("not UTF-8"), "{invalid}");
    std::fs::write(root.join("nul"), b"valid\0binary").map_err(|error| error.to_string())?;
    let binary = call(root, READ_LINES_TOOL, serde_json::json!({"path":"nul"}))
        .err()
        .ok_or("NUL content unexpectedly read")?;
    assert!(binary.contains("binary NUL"), "{binary}");
    std::fs::write(
        root.join("large"),
        vec![b'x'; FILE_EFFECT_MAX_BYTES as usize + 1],
    )
    .map_err(|error| error.to_string())?;
    let large = call(root, READ_LINES_TOOL, serde_json::json!({"path":"large"}))
        .err()
        .ok_or("oversized file unexpectedly read")?;
    assert!(large.contains("byte bound"), "{large}");
    let missing = call(root, READ_LINES_TOOL, serde_json::json!({"path":"absent"}))
        .err()
        .ok_or("missing file unexpectedly read")?;
    assert!(missing.contains("absent"), "{missing}");
    assert!(call(
        root,
        READ_LINES_TOOL,
        serde_json::json!({"path":"../escape"})
    )
    .is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("binary", root.join("link"))
            .map_err(|error| error.to_string())?;
        let linked = call(root, READ_LINES_TOOL, serde_json::json!({"path":"link"}))
            .err()
            .ok_or("link unexpectedly read")?;
        assert!(linked.contains("symlink"), "{linked}");
    }
    Ok(())
}

#[test]
fn nt_search_regex_ignore_negation_and_explicit_override() -> Result<(), String> {
    let scratch = Scratch::new("nt-search")?;
    let root = &scratch.0;
    std::fs::create_dir(root.join("pkg")).map_err(|error| error.to_string())?;
    std::fs::create_dir(root.join("target")).map_err(|error| error.to_string())?;
    std::fs::write(root.join(".gitignore"), "*.gen\n!keep.gen\n")
        .map_err(|error| error.to_string())?;
    std::fs::write(root.join("pkg/.gitignore"), "skip.rs\n").map_err(|error| error.to_string())?;
    for path in [
        "pkg/main.rs",
        "pkg/skip.rs",
        "keep.gen",
        "drop.gen",
        "target/built.rs",
    ] {
        std::fs::write(root.join(path), "Answer 42\n").map_err(|error| error.to_string())?;
    }
    let found = call(
        root,
        GREP_TOOL,
        serde_json::json!({
            "pattern":"answer [0-9]+", "regex":true, "case_sensitive":false
        }),
    )?;
    let paths: Vec<&str> = found["matches"]
        .as_array()
        .ok_or("missing matches")?
        .iter()
        .filter_map(|row| row["path"].as_str())
        .collect();
    assert_eq!(paths, vec!["keep.gen", "pkg/main.rs"]);
    assert!(found["ignoredCount"].as_u64().unwrap_or(0) >= 3);
    let literal = call(
        root,
        GREP_TOOL,
        serde_json::json!({"pattern":"answer [0-9]+"}),
    )?;
    assert_eq!(literal["matches"], serde_json::json!([]));
    let all = call(
        root,
        GREP_TOOL,
        serde_json::json!({
            "pattern":"Answer 42", "include_ignored":true
        }),
    )?;
    assert_eq!(all["matches"].as_array().ok_or("missing matches")?.len(), 4);
    let invalid = call(
        root,
        GREP_TOOL,
        serde_json::json!({"pattern":"[", "regex":true}),
    )
    .err()
    .ok_or("invalid regex unexpectedly accepted")?;
    assert!(invalid.contains("regex is invalid"), "{invalid}");
    Ok(())
}

#[test]
fn nt_list_exposes_directory_kinds_and_truncation() -> Result<(), String> {
    let scratch = Scratch::new("nt-list")?;
    let root = &scratch.0;
    std::fs::create_dir(root.join("src")).map_err(|error| error.to_string())?;
    std::fs::create_dir(root.join("target")).map_err(|error| error.to_string())?;
    std::fs::write(root.join("main.rs"), "main").map_err(|error| error.to_string())?;
    let view = call(root, LIST_TOOL, serde_json::json!({}))?;
    assert_eq!(
        view["entries"],
        serde_json::json!([
            {"path":"main.rs","kind":"file"},
            {"path":"src","kind":"directory"}
        ])
    );
    assert_eq!(view["ignoredCount"], 1);
    let short = call(root, LIST_TOOL, serde_json::json!({"limit":1}))?;
    assert_eq!(short["truncated"], true);
    assert_eq!(short["complete"], false);
    let all = call(root, LIST_TOOL, serde_json::json!({"include_ignored":true}))?;
    assert_eq!(all["entries"].as_array().ok_or("missing entries")?.len(), 2);
    let unavailable = call(root, LIST_TOOL, serde_json::json!({"path":"absent"}))
        .err()
        .ok_or("unavailable directory unexpectedly listed")?;
    assert!(
        unavailable.contains("absent not found; nearest existing parent: ."),
        "{unavailable}"
    );
    assert!(definitions().iter().any(|tool| tool.name == LIST_TOOL));
    assert!(BUILTIN_TOOL_NAMES.contains(&LIST_TOOL));
    Ok(())
}

#[test]
fn nt_patch_preflights_then_applies_multiple_file_kinds() -> Result<(), String> {
    let scratch = Scratch::new("nt-patch")?;
    let root = &scratch.0;
    std::fs::write(root.join("update.rs"), "let answer = 41;\n")
        .map_err(|error| error.to_string())?;
    std::fs::write(root.join("delete.rs"), "old\n").map_err(|error| error.to_string())?;
    std::fs::write(root.join("move.rs"), "move\n").map_err(|error| error.to_string())?;
    let hash = |name: &str| -> Result<String, String> {
        call(root, READ_LINES_TOOL, serde_json::json!({"path":name}))?["sha256"]
            .as_str()
            .map(str::to_owned)
            .ok_or("read_lines hash absent".into())
    };
    let patch = serde_json::json!({"operations":[
        {"op":"create","path":"new.rs","content":"new\n"},
        {"op":"update","path":"update.rs","before_sha256":hash("update.rs")?,"old_text":"41","new_text":"42"},
        {"op":"delete","path":"delete.rs","before_sha256":hash("delete.rs")?},
        {"op":"rename","from":"move.rs","to":"moved.rs","before_sha256":hash("move.rs")?}
    ]});
    let result = call(root, PATCH_TOOL, patch.clone())?;
    assert_eq!(result["complete"], true);
    assert_eq!(
        result["applied"].as_array().ok_or("missing applied")?.len(),
        4
    );
    assert_eq!(
        std::fs::read_to_string(root.join("update.rs")).map_err(|error| error.to_string())?,
        "let answer = 42;\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("new.rs")).map_err(|error| error.to_string())?,
        "new\n"
    );
    assert!(!root.join("delete.rs").exists());
    assert!(!root.join("move.rs").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("moved.rs")).map_err(|error| error.to_string())?,
        "move\n"
    );
    assert!(
        call(root, PATCH_TOOL, patch).is_err(),
        "retry duplicated changes"
    );
    assert!(definitions().iter().any(|tool| tool.name == PATCH_TOOL));
    assert!(BUILTIN_TOOL_NAMES.contains(&PATCH_TOOL));
    Ok(())
}

#[test]
fn nt_patch_rejects_stale_ambiguous_and_escape_before_effect() -> Result<(), String> {
    let scratch = Scratch::new("nt-patch-refusals")?;
    let root = &scratch.0;
    std::fs::write(root.join("src"), "same same\n").map_err(|error| error.to_string())?;
    let hash = call(root, READ_LINES_TOOL, serde_json::json!({"path":"src"}))?["sha256"]
        .as_str()
        .map(str::to_owned)
        .ok_or("hash absent")?;
    for operation in [
        serde_json::json!({"op":"update","path":"src","before_sha256":hash,"old_text":"same","new_text":"new"}),
        serde_json::json!({"op":"delete","path":"src","before_sha256":"0".repeat(64)}),
        serde_json::json!({"op":"create","path":"../escape","content":"bad"}),
        serde_json::json!({"op":"rename","from":"src","to":"../escape","before_sha256":hash}),
    ] {
        let failure = call(
            root,
            PATCH_TOOL,
            serde_json::json!({"operations":[
                {"op":"create","path":"would-create","content":"bad"}, operation
            ]}),
        )
        .err()
        .ok_or("invalid patch accepted")?;
        assert!(!failure.is_empty());
        assert!(
            !root.join("would-create").exists(),
            "preflight applied an earlier operation"
        );
    }
    assert_eq!(
        std::fs::read_to_string(root.join("src")).map_err(|error| error.to_string())?,
        "same same\n"
    );
    Ok(())
}

#[test]
fn nt_patch_partial_failure_reports_applied_and_retry_refuses() -> Result<(), String> {
    let scratch = Scratch::new("nt-patch-partial")?;
    let root = &scratch.0;
    let value = serde_json::json!({"operations":[
        {"op":"create","path":"first","content":"one"},
        {"op":"create","path":"second","content":"two"}
    ]});
    let input = input(value.clone())?;
    let prepared = prepare(root, PATCH_TOOL, &input)?;
    let EffectTarget::Coding(CodingTarget::Patch(_)) = prepared.target else {
        return Err("patch did not use the governed coding effect".into());
    };
    let failure = patch::fail_after_first_for_test(root, &input)
        .err()
        .ok_or("injected failure returned success")?;
    assert!(
        failure.contains("partial failure")
            && failure.contains("first")
            && failure.contains("injected filesystem failure"),
        "{failure}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("first")).map_err(|error| error.to_string())?,
        "one"
    );
    assert!(!root.join("second").exists());
    assert!(
        call(root, PATCH_TOOL, value).is_err(),
        "retry duplicated first create"
    );
    Ok(())
}

#[test]
fn nt_patch_rechecks_source_after_permission_window() -> Result<(), String> {
    let scratch = Scratch::new("nt-patch-approval-race")?;
    let root = &scratch.0;
    std::fs::write(root.join("source.rs"), "let value = 1;\n")
        .map_err(|error| error.to_string())?;
    let hash = call(
        root,
        READ_LINES_TOOL,
        serde_json::json!({"path":"source.rs"}),
    )?["sha256"]
        .as_str()
        .map(str::to_owned)
        .ok_or("source hash absent")?;
    let input = input(serde_json::json!({"operations":[
        {"op":"create","path":"new.rs","content":"new\n"},
        {"op":"update","path":"source.rs","before_sha256":hash,"old_text":"1","new_text":"2"}
    ]}))?;
    let prepared = prepare(root, PATCH_TOOL, &input)?;
    let EffectTarget::Coding(CodingTarget::Patch(plan)) = prepared.target else {
        return Err("patch did not prepare through coding effect".into());
    };
    std::fs::write(root.join("source.rs"), "let value = 3;\n")
        .map_err(|error| error.to_string())?;
    let failure = patch::run(root, &plan, &input)
        .err()
        .ok_or("stale approval unexpectedly applied")?;
    assert!(failure.contains("stale source hash"), "{failure}");
    assert!(
        !root.join("new.rs").exists(),
        "preflight leaked an earlier create"
    );
    Ok(())
}

#[test]
fn fu02_root_overwrite_and_invalid_input_refuse() -> Result<(), String> {
    let scratch = Scratch::new("refusals")?;
    let root = &scratch.0;
    std::fs::write(root.join("existing"), b"preserve").map_err(|error| error.to_string())?;
    // An existing regular file is overwritten (fs.overwrite), not refused.
    for path in ["../escape", "AGENTS.md", ".git", "missing/file"] {
        assert!(
            call(
                root,
                WRITE_TOOL,
                serde_json::json!({"path":path,"content":"bad"})
            )
            .is_err(),
            "accepted {path}"
        );
    }
    assert_eq!(
        std::fs::read(root.join("existing")).map_err(|error| error.to_string())?,
        b"preserve"
    );
    for value in [
        serde_json::json!({"pattern":"../*"}),
        serde_json::json!({"pattern":"**","path":".."}),
        serde_json::json!({"pattern":"**","limit":0}),
        serde_json::json!({"pattern":"**","limit":1001}),
        serde_json::json!({"pattern":"**","path":null}),
        serde_json::json!({"pattern":"**","extra":true}),
    ] {
        assert!(call(root, GLOB_TOOL, value).is_err());
    }
    assert!(call(root, GREP_TOOL, serde_json::json!({"pattern":"a\nb"})).is_err());
    let oversized = call(
        root,
        WRITE_TOOL,
        serde_json::json!({"path":"oversized", "content":"x".repeat(grapt_runtime::TOOL_INPUT_MAX_BYTES)}),
    )
    .err()
    .ok_or("canonical tool input bound survived")?;
    assert!(oversized.contains("tool input is"), "{oversized}");
    assert!(!root.join("oversized").exists());
    Ok(())
}

#[test]
fn fu12_absolute_coding_paths_preserve_workspace_authority() -> Result<(), String> {
    let scratch = Scratch::new("absolute")?;
    let outside = Scratch::new("absolute-outside")?;
    let root = &scratch.0;
    std::fs::create_dir(root.join("src")).map_err(|error| error.to_string())?;
    std::fs::write(root.join("src/answer.py"), "answer = 41\n")
        .map_err(|error| error.to_string())?;
    for path in [root.clone(), root.join("src")] {
        let found = call(
            root,
            GLOB_TOOL,
            serde_json::json!({"path":path,"pattern":"**/*.py","limit":1000}),
        )
        .map_err(|error| format!("contained absolute glob refused: {error}"))?;
        assert_eq!(found["paths"], serde_json::json!(["src/answer.py"]));
        let found = call(
            root,
            GREP_TOOL,
            serde_json::json!({"path":path,"pattern":"answer"}),
        )?;
        assert_eq!(found["matches"][0]["path"], "src/answer.py");
    }
    let target = root.join("src/new.py");
    let args = serde_json::json!({"path":target,"content":"answer = 42\n"});
    let absolute = prepare(root, WRITE_TOOL, &input(args.clone())?)?;
    let relative = prepare(
        root,
        WRITE_TOOL,
        &input(serde_json::json!({"path":"src/new.py","content":"answer = 42\n"}))?,
    )?;
    assert_eq!(absolute.capability, relative.capability);
    assert_eq!(absolute.resource, relative.resource);
    call(root, WRITE_TOOL, args.clone())?;
    assert_eq!(
        std::fs::read(&target).map_err(|error| error.to_string())?,
        b"answer = 42\n"
    );
    // The same absolute path again now names an existing file: an overwrite.
    let replaced = serde_json::json!({"path":target,"content":"answer = 43\n"});
    assert_eq!(
        prepare(root, WRITE_TOOL, &input(replaced.clone())?)?.action,
        "fs.overwrite"
    );
    call(root, WRITE_TOOL, replaced)?;
    assert_eq!(
        std::fs::read(&target).map_err(|error| error.to_string())?,
        b"answer = 43\n"
    );
    for path in [
        outside.0.clone(),
        root.join("src/.."),
        root.join("../escape"),
    ] {
        assert!(call(
            root,
            GLOB_TOOL,
            serde_json::json!({"path":path,"pattern":"**"})
        )
        .is_err());
        assert!(call(
            root,
            GREP_TOOL,
            serde_json::json!({"path":path,"pattern":"answer"})
        )
        .is_err());
        assert!(call(
            root,
            WRITE_TOOL,
            serde_json::json!({"path":path.join("new"),"content":"bad"})
        )
        .is_err());
    }
    for name in [
        "AGENTS.md",
        ".git",
        ".grapt",
        ".stateengine",
        ".codex",
        "CLAUDE.md",
    ] {
        assert!(call(
            root,
            WRITE_TOOL,
            serde_json::json!({"path":root.join(name),"content":"bad"})
        )
        .is_err());
    }
    assert!(!outside.0.join("new").exists());
    Ok(())
}

#[test]
fn fu03_result_and_file_byte_bounds_refuse() -> Result<(), String> {
    let scratch = Scratch::new("bounds")?;
    let root = &scratch.0;
    for name in ["a.txt", "b.txt"] {
        std::fs::write(root.join(name), "needle\n").map_err(|error| error.to_string())?;
    }
    // Past the limit the first page returns, marked truncated, instead of an error.
    let found = call(
        root,
        GLOB_TOOL,
        serde_json::json!({"pattern":"*.txt","limit":1}),
    )?;
    assert_eq!(found["paths"], serde_json::json!(["a.txt"]));
    assert_eq!(found["truncated"], true);
    assert_eq!(found["complete"], false);
    assert_eq!(found["totalMatches"], 2);
    let found = call(
        root,
        GREP_TOOL,
        serde_json::json!({"pattern":"needle","limit":1}),
    )?;
    assert_eq!(found["matches"].as_array().map(Vec::len), Some(1));
    assert_eq!(found["matches"][0]["path"], "a.txt");
    assert_eq!(found["truncated"], true);
    assert_eq!(found["complete"], false);
    let found = call(
        root,
        GREP_TOOL,
        serde_json::json!({"pattern":"needle","limit":2}),
    )?;
    assert_eq!(found["truncated"], false);
    assert_eq!(found["complete"], true);
    std::fs::write(
        root.join("large"),
        vec![b'x'; FILE_EFFECT_MAX_BYTES as usize + 1],
    )
    .map_err(|error| error.to_string())?;
    let omitted = call(
        root,
        GREP_TOOL,
        serde_json::json!({"pattern":"x","glob":"large"}),
    )?;
    assert_eq!(omitted["complete"], false);
    assert_eq!(omitted["skipped"][0]["reason"], "file-byte-bound");
    std::fs::write(root.join("bad"), [0xff]).map_err(|error| error.to_string())?;
    let found = call(
        root,
        GREP_TOOL,
        serde_json::json!({"pattern":"x","glob":"bad"}),
    )?;
    assert_eq!(found["complete"], false);
    assert_eq!(found["skipped"][0]["path"], "bad");
    assert_eq!(found["skipped"][0]["reason"], "binary-or-non-utf8");
    Ok(())
}

#[test]
fn fu04_creation_race_preserves_existing_bytes() -> Result<(), String> {
    let scratch = Scratch::new("race")?;
    let root = &scratch.0;
    let input = input(serde_json::json!({"path":"race","content":"overwrite"}))?;
    let prepared = prepare(root, WRITE_TOOL, &input)?;
    let EffectTarget::Coding(target) = prepared.target else {
        return Err("wrong target".into());
    };
    std::fs::write(root.join("race"), "preserve").map_err(|error| error.to_string())?;
    assert!(run(root, &target, &input).is_err());
    assert_eq!(
        std::fs::read(root.join("race")).map_err(|error| error.to_string())?,
        b"preserve"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn fu05_symlink_escape_refuses_before_effect() -> Result<(), String> {
    let scratch = Scratch::new("link")?;
    let outside = Scratch::new("outside")?;
    std::os::unix::fs::symlink(&outside.0, scratch.0.join("escape"))
        .map_err(|error| error.to_string())?;
    for path in [scratch.0.join("escape"), PathBuf::from("escape")] {
        assert!(call(
            &scratch.0,
            GLOB_TOOL,
            serde_json::json!({"path":path,"pattern":"**"})
        )
        .is_err());
        assert!(call(
            &scratch.0,
            GREP_TOOL,
            serde_json::json!({"path":path,"pattern":"anything"})
        )
        .is_err());
        assert!(call(
            &scratch.0,
            WRITE_TOOL,
            serde_json::json!({"path":path.join("new"),"content":"bad"})
        )
        .is_err());
    }
    assert!(call(
        &scratch.0,
        WRITE_TOOL,
        serde_json::json!({"path":"escape/new","content":"bad"})
    )
    .is_err());
    assert!(call(&scratch.0, GLOB_TOOL, serde_json::json!({"pattern":"**"})).is_err());
    assert!(!outside.0.join("new").exists());
    Ok(())
}

#[test]
fn fu09_depth_scan_and_output_bounds_refuse() -> Result<(), String> {
    let scratch = Scratch::new("more-bounds")?;
    let root = &scratch.0;
    let mut nested = root.clone();
    for _ in 0..33 {
        nested.push("d");
        std::fs::create_dir(&nested).map_err(|error| error.to_string())?;
    }
    // A root-only request must not exhaust itself in unrelated deep directories.
    let narrow = call(root, GLOB_TOOL, serde_json::json!({"pattern":"*.txt"}))?;
    assert_eq!(narrow["paths"], serde_json::json!([]));
    let error = call(root, GLOB_TOOL, serde_json::json!({"pattern":"**"}))
        .err()
        .ok_or("depth bound survived")?;
    assert!(error.contains("depth bound"), "{error}");
    std::fs::create_dir(root.join("scan")).map_err(|error| error.to_string())?;
    for index in 0..17 {
        std::fs::write(
            root.join("scan").join(format!("{index}.txt")),
            vec![b'x'; 1024 * 1024],
        )
        .map_err(|error| error.to_string())?;
    }
    let partial = call(
        root,
        GREP_TOOL,
        serde_json::json!({"path":"scan","pattern":"absent"}),
    )?;
    assert_eq!(partial["complete"], false);
    assert_eq!(partial["truncated"], true);
    assert_eq!(partial["stoppedReason"], "scan-byte-bound");
    std::fs::create_dir(root.join("output")).map_err(|error| error.to_string())?;
    std::fs::write(root.join("output/large.txt"), vec![b'x'; 300 * 1024])
        .map_err(|error| error.to_string())?;
    let found = call(
        root,
        GREP_TOOL,
        serde_json::json!({"path":"output","pattern":"x"}),
    )?;
    assert_eq!(found["matches"][0]["textTruncated"], true);
    assert_eq!(
        found["matches"][0]["text"].as_str().map(str::len),
        Some(1024)
    );
    // Many matching lines stop at the output budget with a truncated page.
    std::fs::write(
        root.join("output/large.txt"),
        format!("{}\n", "x".repeat(1000)).repeat(300),
    )
    .map_err(|error| error.to_string())?;
    let found = call(
        root,
        GREP_TOOL,
        serde_json::json!({"path":"output","pattern":"x","limit":1000}),
    )?;
    assert_eq!(found["truncated"], true);
    let returned = found["matches"].as_array().map_or(0, Vec::len);
    assert!(returned > 0 && returned < 300, "{returned}");
    Ok(())
}

#[cfg(unix)]
#[test]
fn fu10_parent_replaced_by_symlink_after_authorization_refuses() -> Result<(), String> {
    let scratch = Scratch::new("parent-race")?;
    let outside = Scratch::new("parent-outside")?;
    let root = &scratch.0;
    std::fs::create_dir(root.join("parent")).map_err(|error| error.to_string())?;
    let input = input(serde_json::json!({"path":"parent/new","content":"bad"}))?;
    let EffectTarget::Coding(target) = prepare(root, WRITE_TOOL, &input)?.target else {
        return Err("wrong target".into());
    };
    std::fs::remove_dir(root.join("parent")).map_err(|error| error.to_string())?;
    std::os::unix::fs::symlink(&outside.0, root.join("parent"))
        .map_err(|error| error.to_string())?;
    assert!(run(root, &target, &input).is_err());
    assert!(!outside.0.join("new").exists());
    Ok(())
}

#[test]
fn fu13_real_workspace_exclusions_and_binary_skips() -> Result<(), String> {
    let scratch = Scratch::new("real-workspace")?;
    let root = &scratch.0;
    std::fs::create_dir(root.join("target")).map_err(|error| error.to_string())?;
    for index in 0..10_001 {
        std::fs::write(
            root.join("target").join(format!("{index}.rs")),
            b"generated",
        )
        .map_err(|error| error.to_string())?;
    }
    std::fs::create_dir(root.join("ignored")).map_err(|error| error.to_string())?;
    std::fs::write(root.join("ignored/private.txt"), b"ignored fixture")
        .map_err(|error| error.to_string())?;
    std::fs::write(root.join(".gitignore"), b"ignored/\n").map_err(|error| error.to_string())?;
    std::fs::write(root.join("source.rs"), b"needle").map_err(|error| error.to_string())?;
    std::fs::write(root.join("binary"), [0xff, 0x00]).map_err(|error| error.to_string())?;
    let default = call(root, GLOB_TOOL, serde_json::json!({"pattern":"**/*.rs"}))?;
    assert_eq!(default["paths"], serde_json::json!(["source.rs"]));
    assert_eq!(default["complete"], true);
    assert_eq!(default["ignoredCount"], 3);
    let broad = call(
        root,
        GLOB_TOOL,
        serde_json::json!({"pattern":"**/*.rs","include_ignored":true}),
    )?;
    assert_eq!(broad["paths"], serde_json::json!(["source.rs"]));
    assert_eq!(broad["complete"], true);
    let failure = call(
        root,
        GLOB_TOOL,
        serde_json::json!({"path":"target","pattern":"**/*.rs"}),
    )
    .err()
    .ok_or("explicit large scope bypassed entry bound")?;
    assert!(
        failure.contains("target") && failure.contains("entry bound"),
        "{failure}"
    );
    let found = call(
        root,
        GLOB_TOOL,
        serde_json::json!({"pattern":"**/*.rs","include_ignored":true,"exclude":["target/**"]}),
    )
    .map_err(|error| format!("build exclusion failed: {error}"))?;
    assert_eq!(found["paths"], serde_json::json!(["source.rs"]));
    assert_eq!(found["complete"], false);
    assert_eq!(found["skipped"][0]["reason"], "excluded");
    let found = call(
        root,
        GREP_TOOL,
        serde_json::json!({"pattern":"needle","include_ignored":true,"exclude":["target/**"]}),
    )?;
    assert_eq!(found["matches"][0]["path"], "source.rs");
    assert_eq!(found["complete"], false, "skipped files reported complete");
    let skipped = found["skipped"].as_array().ok_or("missing skipped set")?;
    assert!(skipped
        .iter()
        .any(|row| row["path"] == "binary" && row["reason"] == "binary-or-non-utf8"));
    assert!(skipped
        .iter()
        .any(|row| row["path"] == "target" && row["reason"] == "excluded"));
    for excluded in [
        serde_json::json!(["../escape"]),
        serde_json::json!(vec!["**"; 33]),
        serde_json::json!(null),
    ] {
        assert!(call(
            root,
            GLOB_TOOL,
            serde_json::json!({"pattern":"**","exclude":excluded})
        )
        .is_err());
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn fu14_non_regular_and_non_utf8_entries_are_reported() -> Result<(), String> {
    use std::os::unix::ffi::OsStringExt;
    let scratch = Scratch::new("skipped-entries")?;
    let root = &scratch.0;
    std::fs::write(root.join("good"), b"needle").map_err(|error| error.to_string())?;
    std::fs::write(
        root.join(std::ffi::OsString::from_vec(vec![0xff])),
        b"needle",
    )
    .map_err(|error| error.to_string())?;
    std::os::unix::fs::symlink(root.join("missing"), root.join("link"))
        .map_err(|error| error.to_string())?;
    // /proc's directory handle keeps this witness below sockaddr_un's path
    // ceiling even when the governed scratch directory has a long name.
    #[cfg(target_os = "linux")]
    let socket = {
        use std::os::fd::AsRawFd;
        let directory = std::fs::File::open(root).map_err(|error| error.to_string())?;
        std::os::unix::net::UnixListener::bind(format!(
            "/proc/self/fd/{}/socket",
            directory.as_raw_fd()
        ))
        .map_err(|error| error.to_string())?
    };
    let found = call(root, GREP_TOOL, serde_json::json!({"pattern":"needle"}))?;
    assert_eq!(
        found["matches"].as_array().ok_or("matches absent")?.len(),
        1
    );
    assert_eq!(found["complete"], false);
    let skipped = found["skipped"].as_array().ok_or("skipped absent")?;
    for reason in ["filename-not-utf8", "symlink-or-reparse-point"] {
        assert!(
            skipped.iter().any(|row| row["reason"] == reason),
            "missing {reason}"
        );
    }
    #[cfg(target_os = "linux")]
    {
        assert!(skipped.iter().any(|row| row["reason"] == "non-regular"));
        drop(socket);
    }
    Ok(())
}

#[test]
fn fu15_skipped_report_is_bounded() -> Result<(), String> {
    let scratch = Scratch::new("skipped-bound")?;
    for index in 0..1001 {
        std::fs::write(scratch.0.join(format!("{index}.bin")), [0xff])
            .map_err(|error| error.to_string())?;
    }
    let result = call(
        &scratch.0,
        GREP_TOOL,
        serde_json::json!({"pattern":"needle"}),
    )?;
    assert_eq!(result["complete"], false);
    assert_eq!(result["truncated"], true);
    assert_eq!(result["stoppedReason"], "skipped-report-bound");
    let skipped = result["skipped"]
        .as_array()
        .ok_or("skipped report absent")?;
    assert_eq!(skipped.len(), 1000, "skipped report bound changed");
    assert!(skipped.iter().all(|row| {
        row["path"]
            .as_str()
            .is_some_and(|path| path.ends_with(".bin"))
            && row["reason"] == "binary-or-non-utf8"
    }));
    Ok(())
}
