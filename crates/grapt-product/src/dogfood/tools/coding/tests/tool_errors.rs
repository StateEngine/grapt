use super::*;

#[test]
fn tool01b_binary_and_oversized_files_do_not_erase_text_matches() -> Result<(), String> {
    let scratch = Scratch::new("tool-errors-binary")?;
    let root = &scratch.0;
    std::fs::write(root.join("a.txt"), "needle\n").map_err(|error| error.to_string())?;
    std::fs::write(root.join("archive.tar.xz"), vec![0_u8; 1024 * 1024 + 1])
        .map_err(|error| error.to_string())?;
    // More than the aggregate text budget, but each binary needs only a probe.
    for index in 0..17 {
        std::fs::write(
            root.join(format!("binary-{index:02}")),
            vec![0_u8; 1024 * 1024],
        )
        .map_err(|error| error.to_string())?;
    }
    std::fs::write(root.join("z.txt"), "needle\n").map_err(|error| error.to_string())?;
    let result = call(root, GREP_TOOL, serde_json::json!({"pattern":"needle"}))?;
    let paths: Vec<_> = result["matches"]
        .as_array()
        .ok_or("matches absent")?
        .iter()
        .map(|row| row["path"].as_str())
        .collect();
    assert_eq!(
        paths,
        [Some("a.txt"), Some("z.txt")],
        "omissions lost text matches"
    );
    assert_eq!(result["complete"], false);
    assert_eq!(result["truncated"], false);
    let skipped = result["skipped"]
        .as_array()
        .ok_or("omission details absent")?;
    assert_eq!(skipped.len(), 18);
    assert!(skipped
        .iter()
        .any(|row| row["path"] == "archive.tar.xz" && row["reason"] == "file-byte-bound"));
    assert!(skipped
        .iter()
        .any(|row| row["reason"] == "binary-or-non-utf8"));
    assert!(result["scannedBytes"].as_u64().ok_or("scan count absent")? < 128 * 1024);
    assert!(result["hint"]
        .as_str()
        .is_some_and(|hint| hint.contains("partial")));
    Ok(())
}

#[test]
fn tool01b_a_multibyte_character_across_the_probe_is_still_searched() -> Result<(), String> {
    let scratch = Scratch::new("tool-errors-utf8")?;
    std::fs::write(
        scratch.0.join("source.txt"),
        format!("{}界needle\n", "a".repeat(4095)),
    )
    .map_err(|error| error.to_string())?;
    let result = call(
        &scratch.0,
        GREP_TOOL,
        serde_json::json!({"pattern":"needle"}),
    )?;
    assert_eq!(result["matches"].as_array().map(Vec::len), Some(1));
    assert_eq!(result["complete"], true);
    Ok(())
}

#[test]
fn tool01b_aggregate_budget_returns_prior_matches_and_named_omission() -> Result<(), String> {
    let scratch = Scratch::new("tool-errors-budget")?;
    for index in 0..17 {
        let mut content = vec![b'x'; 1024 * 1024];
        content[..7].copy_from_slice(b"needle\n");
        std::fs::write(scratch.0.join(format!("{index:02}.txt")), content)
            .map_err(|error| error.to_string())?;
    }
    let result = call(
        &scratch.0,
        GREP_TOOL,
        serde_json::json!({"pattern":"needle"}),
    )?;
    assert!(
        !result["matches"]
            .as_array()
            .ok_or("matches absent")?
            .is_empty(),
        "aggregate bound erased earlier matches"
    );
    assert_eq!(result["complete"], false);
    assert_eq!(result["truncated"], true);
    assert_eq!(result["stoppedReason"], "scan-byte-bound");
    assert!(result["skipped"]
        .as_array()
        .ok_or("skipped absent")?
        .iter()
        .any(|row| row["reason"] == "scan-byte-bound"));
    assert!(result["scannedBytes"].as_u64().ok_or("scan count absent")? <= 16 * 1024 * 1024);
    Ok(())
}
