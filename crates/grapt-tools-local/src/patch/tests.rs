use super::{parse_patch, updated, Hunk};

#[test]
fn text_patch_headers_parse_multiple_files_and_move_with_context() -> Result<(), String> {
    let patch = parse_patch("*** Begin Patch\n*** Add File: new.txt\n+hello\n+world\n*** Delete File: old.txt\n*** Update File: from.rs\n*** Move to: to.rs\n@@ fn work()\n-old\n+new\n keep\n*** End Patch\n").map_err(|e|e.to_string())?;
    assert_eq!(patch.hunks.len(), 3);
    let Hunk::Add { path, contents } = &patch.hunks[0] else {
        return Err("add missing".into());
    };
    assert_eq!(path.to_str(), Some("new.txt"));
    assert_eq!(contents, "hello\nworld\n");
    let Hunk::Update {
        path,
        move_path,
        chunks,
    } = &patch.hunks[2]
    else {
        return Err("update missing".into());
    };
    assert_eq!(path.to_str(), Some("from.rs"));
    assert_eq!(move_path.as_deref().and_then(|p| p.to_str()), Some("to.rs"));
    assert_eq!(
        updated("fn work()\nold\nkeep\n", "from.rs", chunks)?,
        "fn work()\nnew\nkeep\n"
    );
    Ok(())
}

fn apply(source: &str, body: &str) -> Result<String, String> {
    let patch = parse_patch(&format!(
        "*** Begin Patch\n*** Update File: file\n{body}\n*** End Patch"
    ))
    .map_err(|e| e.to_string())?;
    let Some(Hunk::Update { chunks, .. }) = patch.hunks.first() else {
        return Err("update missing".into());
    };
    updated(source, "file", chunks)
}

#[test]
fn contextual_matching_keeps_unchanged_bytes_and_existing_line_endings() -> Result<(), String> {
    assert_eq!(
        apply(
            "  def f():\r\n    old\t\n  end\r\n",
            "@@ def f():\n-    old\n+    new\n end"
        )?,
        "  def f():\r\n    new\r\n  end\r\n"
    );
    assert_eq!(
        apply("before\nold\nafter", "@@\n before\n-old\n+new\n after")?,
        "before\nnew\nafter\n"
    );
    assert_eq!(apply("", "@@\n+first")?, "first\n");
    Ok(())
}

#[test]
fn an_eof_chunk_prefers_the_file_ending_and_later_context_advances() -> Result<(), String> {
    assert_eq!(
        apply("x\nend\nx\nend\n", "@@\n-x\n+last\n end\n*** End of File")?,
        "x\nend\nlast\nend\n"
    );
    assert_eq!(
        apply("fn a\nx\nfn b\nx\n", "@@ fn b\n-x\n+chosen")?,
        "fn a\nx\nfn b\nchosen\n"
    );
    Ok(())
}

#[test]
fn text_patch_unicode_and_whitespace_matching_work_without_rewriting_context() -> Result<(), String>
{
    assert_eq!(
        apply("say “hello”—now\n", "@@\n-say \"hello\"-now\n+say ok")?,
        "say ok\n"
    );
    assert!(apply("different\n", "@@\n-not here\n+new").is_err());
    Ok(())
}

#[test]
fn malformed_or_oversized_text_is_refused_before_filesystem_work() {
    for patch in [
        "not a patch",
        "*** Begin Patch\n*** Update File: empty\n*** End Patch",
        "*** Begin Patch\n*** Add File: file\nunprefixed\n*** End Patch",
        "*** Begin Patch\n*** Update File: file\n@@\n*** End Patch",
    ] {
        assert!(parse_patch(patch).is_err(), "accepted {patch}");
    }
    assert!(parse_patch(&"x".repeat(256 * 1024 + 1)).is_err());
    let mut patch = "*** Begin Patch\n".to_owned();
    for n in 0..17 {
        patch.push_str(&format!("*** Add File: {n}\n+line\n"));
    }
    patch.push_str("*** End Patch");
    assert!(parse_patch(&patch).is_err());
    assert!(apply(&"x".repeat(1024 * 1024 + 1), "@@\n+x").is_err());
}

#[test]
fn repetitive_sources_cannot_hold_the_effect_loop_in_quadratic_matching() {
    let mut body = "@@\n".to_owned();
    for _ in 0..99 {
        body.push_str("-r\n");
    }
    body.push_str("-missing\n+replacement");
    let result = apply(&"r\n".repeat(400_000), &body);
    assert!(result.is_err_and(|error| error.contains("matching work exceeded")));
}

#[test]
fn lenient_heredoc_and_environment_preamble_follow_text_patch_parsing() -> Result<(), String> {
    let patch=parse_patch("<<'EOF'\n*** Begin Patch\n*** Environment ID: primary\n*** Add File: empty\n*** End Patch\nEOF").map_err(|e|e.to_string())?;
    assert_eq!(patch.environment_id.as_deref(), Some("primary"));
    assert_eq!(patch.hunks.len(), 1);
    Ok(())
}
