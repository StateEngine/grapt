//! Search and edit results a model can act on without a retry.
use super::*;

fn edit(root: &Path, value: serde_json::Value) -> Result<serde_json::Value, String> {
    let input = input(value)?;
    let path = input_path(&input)?;
    let receipt = run_edit(root, &root.join(path), &input)?;
    serde_json::from_slice(&receipt).map_err(|error| error.to_string())
}

fn input_path(input: &BoundedToolInput) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_slice(input.as_bytes()).map_err(|error| error.to_string())?;
    value["path"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "edit input has no path".to_owned())
}

#[test]
fn grep_path_may_name_one_file() -> Result<(), String> {
    let scratch = Scratch::new("grep-file")?;
    let root = &scratch.0;
    std::fs::create_dir(root.join("src")).map_err(|error| error.to_string())?;
    std::fs::write(root.join("src/lib.rs"), "fn one() {}\nfn two() {}\n")
        .map_err(|error| error.to_string())?;
    std::fs::write(root.join("src/other.rs"), "fn one() {}\n")
        .map_err(|error| error.to_string())?;
    let found = call(
        root,
        GREP_TOOL,
        serde_json::json!({"path":"src/lib.rs","pattern":"fn "}),
    )?;
    assert_eq!(
        found["matches"],
        serde_json::json!([
            {"path":"src/lib.rs","line":1,"text":"fn one() {}"},
            {"path":"src/lib.rs","line":2,"text":"fn two() {}"},
        ])
    );
    assert_eq!(found["complete"], true);
    std::fs::create_dir(root.join(".git")).map_err(|error| error.to_string())?;
    std::fs::write(root.join(".git/config"), "secret").map_err(|error| error.to_string())?;
    let refused = call(
        root,
        GREP_TOOL,
        serde_json::json!({"path":".git/config","pattern":"secret"}),
    )
    .err()
    .ok_or("grep read protected control metadata")?;
    assert!(refused.contains("protected control metadata"), "{refused}");
    Ok(())
}

#[test]
fn read_lines_defaults_to_a_useful_window_and_pages_at_the_byte_budget() -> Result<(), String> {
    let scratch = Scratch::new("read-window")?;
    let root = &scratch.0;
    let short: String = (1..=1000).map(|line| format!("line {line}\n")).collect();
    std::fs::write(root.join("short.txt"), short).map_err(|error| error.to_string())?;
    let view = call(
        root,
        READ_LINES_TOOL,
        serde_json::json!({"path":"short.txt"}),
    )?;
    assert_eq!(view["lines"].as_array().map(Vec::len), Some(400));
    assert_eq!(view["nextLine"], 401);
    let view = call(
        root,
        READ_LINES_TOOL,
        serde_json::json!({"path":"short.txt","line_count":2000}),
    )?;
    assert_eq!(view["lines"].as_array().map(Vec::len), Some(1000));
    assert_eq!(view["complete"], true);

    // 1000 wide lines exceed the page budget: the page ends early and says where to resume.
    let wide = format!("{}\n", "w".repeat(500)).repeat(1000);
    std::fs::write(root.join("wide.txt"), wide).map_err(|error| error.to_string())?;
    let view = call(
        root,
        READ_LINES_TOOL,
        serde_json::json!({"path":"wide.txt","line_count":1000}),
    )?;
    let returned = view["lines"].as_array().map_or(0, Vec::len);
    assert!(returned > 100 && returned < 1000, "{returned}");
    assert_eq!(view["nextLine"], returned + 1);
    assert_eq!(view["complete"], false);
    let next = call(
        root,
        READ_LINES_TOOL,
        serde_json::json!({"path":"wide.txt","start_line":returned + 1,"line_count":1}),
    )?;
    assert_eq!(next["lines"][0]["line"], returned + 1);
    Ok(())
}

#[test]
fn read_lines_pages_a_single_long_line_without_losing_unicode() -> Result<(), String> {
    let scratch = Scratch::new("read-long-line")?;
    let root = &scratch.0;
    std::fs::create_dir(root.join("src")).map_err(|error| error.to_string())?;
    std::fs::write(
        root.join("src/AGENTS.md"),
        "Preserve generated source exactly.\n",
    )
    .map_err(|error| error.to_string())?;
    let source = "α\"\t".repeat(90_000);
    std::fs::write(root.join("src/generated.txt"), format!("{source}\nNEXT\n"))
        .map_err(|error| error.to_string())?;
    let mut input = serde_json::json!({"path":"src/generated.txt","line_count":1});
    let mut recovered = String::new();
    let mut expected_byte = 0;
    let mut digest = None;
    let mut pages = 0;
    for _ in 0..10 {
        let view = call(root, READ_LINES_TOOL, input.clone())?;
        assert!(view.to_string().len() <= OUTPUT_BYTES);
        assert_eq!(view.get("scopedInstructions").is_some(), pages == 0);
        let row = &view["lines"][0];
        assert_eq!(row["line"], 1);
        assert_eq!(row["byteStart"], expected_byte);
        let text = row["text"].as_str().ok_or("long-line text absent")?;
        assert!(!text.is_empty(), "long-line page made no progress");
        recovered.push_str(text);
        expected_byte = row["byteEnd"].as_u64().ok_or("byte cursor absent")?;
        let sha = view["sha256"].as_str().ok_or("source digest absent")?;
        if let Some(previous) = &digest {
            assert_eq!(sha, previous);
        } else {
            digest = Some(sha.to_owned());
        }
        pages += 1;
        assert_eq!(view["complete"], false);
        if view["nextLine"] == 2 {
            assert_eq!(view["nextColumn"], 1);
            break;
        }
        assert_eq!(view["nextLine"], 1);
        let column = view["nextColumn"].as_u64().ok_or("column cursor absent")?;
        assert!(column > 1);
        input["start_column"] = column.into();
    }
    assert!(pages > 1);
    assert_eq!(
        recovered, source,
        "paging lost or duplicated source characters"
    );
    let tail = call(
        root,
        READ_LINES_TOOL,
        serde_json::json!({"path":"src/generated.txt","start_line":2}),
    )?;
    assert_eq!(tail["lines"][0]["text"], "NEXT");
    assert_eq!(tail["complete"], true);
    Ok(())
}

#[test]
fn read_lines_continuation_leaves_text_at_a_byte_offset_digit_boundary() -> Result<(), String> {
    let scratch = Scratch::new("read-offset-boundary")?;
    let root = &scratch.0;
    // The newline crosses from a six-digit to a seven-digit byteEnd. A
    // fragment containing all visible text would otherwise fit while the
    // whole row does not, yielding an empty continuation.
    let overhead = serde_json::json!({
        "line":2, "byteStart":800_000, "byteEnd":1_000_000, "text":"",
    })
    .to_string()
    .len();
    let source = "w".repeat(OUTPUT_BYTES * 3 / 4 - overhead);
    let offset = 999_999 - source.len();
    let content = format!("{}\n{source}\n", "p".repeat(offset - 1));
    std::fs::write(root.join("boundary.txt"), content).map_err(|error| error.to_string())?;
    let first = call(
        root,
        READ_LINES_TOOL,
        serde_json::json!({"path":"boundary.txt","start_line":2}),
    )?;
    let prefix = first["lines"][0]["text"].as_str().ok_or("prefix absent")?;
    assert!(!prefix.is_empty() && prefix.len() < source.len());
    assert_eq!(first["nextLine"], 2);
    let next = call(
        root,
        READ_LINES_TOOL,
        serde_json::json!({
            "path":"boundary.txt", "start_line":2, "start_column":first["nextColumn"],
        }),
    )?;
    let suffix = next["lines"][0]["text"].as_str().ok_or("suffix absent")?;
    assert!(!suffix.is_empty());
    assert_eq!(format!("{prefix}{suffix}"), source);
    assert_eq!(next["complete"], true);
    Ok(())
}

#[test]
fn read_lines_column_cursor_uses_characters_and_rejects_past_end() -> Result<(), String> {
    let scratch = Scratch::new("read-column")?;
    let root = &scratch.0;
    std::fs::write(root.join("text.txt"), "αβtail\nnext\n").map_err(|error| error.to_string())?;
    let view = call(
        root,
        READ_LINES_TOOL,
        serde_json::json!({"path":"text.txt","start_column":2,"line_count":1}),
    )?;
    assert_eq!(view["lines"][0]["text"], "βtail");
    assert_eq!(view["lines"][0]["byteStart"], 2);
    assert_eq!(view["lines"][0]["byteEnd"], 9);
    assert_eq!(view["nextLine"], 2);
    assert_eq!(view["nextColumn"], 1);
    for column in [0, 8, 1_048_578] {
        assert!(call(
            root,
            READ_LINES_TOOL,
            serde_json::json!({"path":"text.txt","start_column":column})
        )
        .is_err());
    }
    let error = call(
        root,
        READ_LINES_TOOL,
        serde_json::json!({"path":"text.txt","start_column":8}),
    )
    .err()
    .ok_or("past-end column accepted")?;
    assert!(error.contains("start_column is past the end"), "{error}");
    let tail = call(
        root,
        READ_LINES_TOOL,
        serde_json::json!({"path":"text.txt","start_line":2}),
    )?;
    assert_eq!(tail["nextLine"], serde_json::Value::Null);
    assert_eq!(tail["nextColumn"], serde_json::Value::Null);
    Ok(())
}

#[test]
fn edit_reports_absent_and_ambiguous_text_distinctly_and_replace_all_is_deliberate(
) -> Result<(), String> {
    let scratch = Scratch::new("edit-all")?;
    let root = &scratch.0;
    let path = root.join("names.rs");
    std::fs::write(&path, "let old = 1;\nkeep();\nuse_it(old);\n")
        .map_err(|error| error.to_string())?;

    let absent = edit(
        root,
        serde_json::json!({"path":"names.rs","old_text":"missing","new_text":"x"}),
    )
    .err()
    .ok_or("absent old_text edited")?;
    assert!(absent.contains("not found"), "{absent}");
    let ambiguous = edit(
        root,
        serde_json::json!({"path":"names.rs","old_text":"old","new_text":"new"}),
    )
    .err()
    .ok_or("ambiguous old_text edited")?;
    assert!(ambiguous.contains("occurs 2 times"), "{ambiguous}");
    assert!(ambiguous.contains("replace_all"), "{ambiguous}");
    assert_eq!(
        std::fs::read_to_string(&path).map_err(|error| error.to_string())?,
        "let old = 1;\nkeep();\nuse_it(old);\n",
        "a refused edit wrote the file"
    );

    let receipt = edit(
        root,
        serde_json::json!({"path":"names.rs","old_text":"old","new_text":"new","replace_all":true}),
    )?;
    assert_eq!(receipt["exactReplacements"], 2);
    assert_eq!(
        std::fs::read_to_string(&path).map_err(|error| error.to_string())?,
        "let new = 1;\nkeep();\nuse_it(new);\n"
    );
    // One hunk covers the first through last replacement, unchanged lines included.
    assert_eq!(
        receipt["files"][0]["patch"],
        "--- names.rs\n+++ names.rs\n@@ -1,3 +1,3 @@\n\
         -let old = 1;\n-keep();\n-use_it(old);\n\
         +let new = 1;\n+keep();\n+use_it(new);\n"
    );

    let patch = call(
        root,
        PATCH_TOOL,
        serde_json::json!({"operations":[{"op":"update","path":"names.rs",
            "before_sha256":call(root, READ_LINES_TOOL, serde_json::json!({"path":"names.rs"}))?["sha256"],
            "old_text":"new","new_text":"x"}]}),
    )
    .err()
    .ok_or("ambiguous patch anchor applied")?;
    assert!(patch.contains("occurs 2 times"), "{patch}");
    Ok(())
}

#[test]
fn replace_all_is_sized_before_it_is_built() -> Result<(), String> {
    let scratch = Scratch::new("edit-size")?;
    let root = &scratch.0;
    let path = root.join("many.txt");
    let width = usize::try_from(FILE_EFFECT_MAX_BYTES / 1024).map_err(|error| error.to_string())?;
    // 1024 replacements of `width` bytes land exactly on the file bound.
    std::fs::write(&path, "a".repeat(1024)).map_err(|error| error.to_string())?;
    let receipt = edit(
        root,
        serde_json::json!({"path":"many.txt","old_text":"a","new_text":"x".repeat(width),"replace_all":true}),
    )?;
    assert_eq!(receipt["afterBytes"], FILE_EFFECT_MAX_BYTES);
    // One more byte is refused before anything is built or written; so is a
    // replacement whose product would be enormous.
    for (content, new_text) in [
        (format!("{}b", "a".repeat(1024)), "x".repeat(width) + "x"),
        ("a".repeat(1024), "x".repeat(200_000)),
    ] {
        std::fs::write(&path, &content).map_err(|error| error.to_string())?;
        let refused = edit(
            root,
            serde_json::json!({"path":"many.txt","old_text":"a","new_text":new_text,"replace_all":true}),
        )
        .err()
        .ok_or("oversized replace_all was written")?;
        assert!(refused.contains("file bound"), "{refused}");
        assert_eq!(
            std::fs::read_to_string(&path).map_err(|error| error.to_string())?,
            content
        );
    }
    Ok(())
}

#[test]
fn grep_page_with_many_skips_still_fits_the_output_bound() -> Result<(), String> {
    let scratch = Scratch::new("grep-envelope")?;
    let root = &scratch.0;
    // Excluded-file rows (189 bytes with separator) fill the skipped half;
    // ~930 short match rows fill the other, their separators alone ~1 KiB.
    std::fs::write(
        root.join("found.txt"),
        format!("{}\n", "f".repeat(100)).repeat(1000),
    )
    .map_err(|error| error.to_string())?;
    for index in 0..693 {
        let name = format!("skip{index:03}{}", "s".repeat(150));
        std::fs::write(root.join(name), "").map_err(|error| error.to_string())?;
    }
    let found = call(
        root,
        GREP_TOOL,
        serde_json::json!({"pattern":"f","limit":1000,"exclude":["skip*"]}),
    )?;
    assert_eq!(found["skipped"].as_array().map(Vec::len), Some(693));
    assert_eq!(found["truncated"], true);
    let returned = found["matches"].as_array().map_or(0, Vec::len);
    assert!(returned > 0 && returned < 1000, "{returned}");
    let encoded = serde_json::to_vec(&found).map_err(|error| error.to_string())?;
    assert!(encoded.len() <= OUTPUT_BYTES, "{}", encoded.len());
    Ok(())
}

#[test]
fn braces_and_classes_reach_glob_grep_and_exclude_alike() -> Result<(), String> {
    let scratch = Scratch::new("glob-grammar")?;
    let root = &scratch.0;
    std::fs::create_dir(root.join("src")).map_err(|error| error.to_string())?;
    for name in ["app.ts", "view.tsx", "util.js", "Main.rs", "main.rs"] {
        std::fs::write(root.join("src").join(name), "needle\n")
            .map_err(|error| error.to_string())?;
    }
    let found = call(
        root,
        GLOB_TOOL,
        serde_json::json!({"pattern":"**/*.{ts,tsx}"}),
    )?;
    assert_eq!(
        found["paths"],
        serde_json::json!(["src/app.ts", "src/view.tsx"])
    );
    let found = call(
        root,
        GLOB_TOOL,
        serde_json::json!({"pattern":"src/[A-Z]*.rs"}),
    )?;
    assert_eq!(found["paths"], serde_json::json!(["src/Main.rs"]));
    let grep = call(
        root,
        GREP_TOOL,
        serde_json::json!({"pattern":"needle", "glob":"**/*.{rs,js}", "exclude":["**/[M]*.rs"]}),
    )?;
    let paths: Vec<&str> = grep["matches"]
        .as_array()
        .map(|rows| rows.iter().filter_map(|row| row["path"].as_str()).collect())
        .unwrap_or_default();
    assert_eq!(paths, ["src/main.rs", "src/util.js"], "{grep}");
    Ok(())
}

#[test]
fn a_descending_class_range_is_refused_by_glob_grep_and_exclude() -> Result<(), String> {
    let scratch = Scratch::new("glob-range")?;
    let root = &scratch.0;
    std::fs::write(root.join("m.rs"), "needle\n").map_err(|error| error.to_string())?;
    for (tool, value) in [
        (GLOB_TOOL, serde_json::json!({"pattern":"[z-a].rs"})),
        (
            GREP_TOOL,
            serde_json::json!({"pattern":"needle", "glob":"[z-a].rs"}),
        ),
        (
            GREP_TOOL,
            serde_json::json!({"pattern":"needle", "exclude":["[!z-a]*"]}),
        ),
    ] {
        let refused = call(root, tool, value.clone()).err().unwrap_or_default();
        assert!(
            refused.contains("range z-a is descending"),
            "{value}: {refused}"
        );
    }
    Ok(())
}
