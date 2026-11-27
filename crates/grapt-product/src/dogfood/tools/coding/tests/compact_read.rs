use super::*;

fn text(root: &Path, value: serde_json::Value) -> Result<String, String> {
    String::from_utf8(call_bytes(root, READ_LINES_TOOL, value)?).map_err(|e| e.to_string())
}

#[test]
fn compact_read_preserves_source_and_patch_identity() -> Result<(), String> {
    let scratch = Scratch::new("compact-read")?;
    let root = &scratch.0;
    std::fs::write(root.join("source.rs"), "α\r\n\nβeta\nlast").map_err(|e| e.to_string())?;
    let compact = text(root, serde_json::json!({"path":"source.rs"}))?;
    let structured = call(
        root,
        READ_LINES_TOOL,
        serde_json::json!({"path":"source.rs","structured":true}),
    )?;
    let hash = structured["sha256"].as_str().ok_or("hash absent")?;
    assert!(compact.contains(&format!("sha256: {hash}")));
    assert!(compact.contains("1→α\n2→\n3→βeta\n4→last\n"));
    let patch = serde_json::json!({"operations":[{
        "op":"update","path":"source.rs","before_sha256":hash,"old_text":"βeta","new_text":"fixed"
    }]});
    assert_eq!(call(root, PATCH_TOOL, patch.clone())?["complete"], true);
    assert!(
        call(root, PATCH_TOOL, patch).is_err(),
        "stale compact hash accepted"
    );
    Ok(())
}

#[test]
fn compact_read_continuations_are_lossless_and_keep_scoped_instructions() -> Result<(), String> {
    let scratch = Scratch::new("compact-read-pages")?;
    let root = &scratch.0;
    std::fs::create_dir(root.join("src")).map_err(|e| e.to_string())?;
    std::fs::write(
        root.join("src/AGENTS.md"),
        "Preserve generated source exactly.\n",
    )
    .map_err(|e| e.to_string())?;
    let source = "α\"\t".repeat(90_000);
    std::fs::write(root.join("src/long.txt"), format!("{source}\nNEXT\n"))
        .map_err(|e| e.to_string())?;
    let mut args = serde_json::json!({"path":"src/long.txt","line_count":1});
    let mut recovered = String::new();
    for page in 0..10 {
        let compact = text(root, args.clone())?;
        let row = compact
            .lines()
            .find_map(|line| line.strip_prefix("1→"))
            .ok_or("page absent")?;
        recovered.push_str(row);
        assert_eq!(compact.contains("[scoped instructions]"), page == 0);
        if page == 0 {
            assert!(compact.contains("Preserve generated source exactly."));
        }
        let cursor = compact
            .lines()
            .find_map(|line| line.strip_prefix("[continue: start_line="))
            .and_then(|cursor| cursor.strip_suffix(']'))
            .ok_or("continuation absent")?;
        let (line, column) = cursor.split_once(" start_column=").ok_or("column absent")?;
        if line == "2" {
            break;
        }
        args["start_column"] = column.parse::<u64>().map_err(|e| e.to_string())?.into();
    }
    assert_eq!(recovered, source);
    Ok(())
}
