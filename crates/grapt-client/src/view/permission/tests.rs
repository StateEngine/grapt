use super::*;

#[test]
fn compact_approval_keeps_resource_metadata_in_expanded_review(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut request = ask(serde_json::json!({"type":"shell","command":"git status",
        "cwd":"/private-context","shell":"fixture-shell","truncated":false}))?;
    request.title = "shell: git status\nworkdir: /private-context\nshell: fixture-shell".into();
    let compact = compact_rows(&request, None, 200);
    assert!(!compact
        .iter()
        .any(|(line, _)| line.contains("/private-context") || line.contains("fixture-shell")));
    assert!(compact
        .iter()
        .any(|(line, _)| line.contains("modifies production")));
    let expanded = review_rows(&request, 200);
    for context in ["/private-context", "fixture-shell"] {
        assert!(expanded.iter().any(|(line, _)| line.contains(context)));
    }
    Ok(())
}

fn ask(preview: serde_json::Value) -> Result<PermissionAsk, Box<dyn std::error::Error>> {
    Ok(PermissionAsk {
        session_id: crate::wire::session_id("ses_preview")?,
        request_id: crate::wire::permission_id("per_preview")?,
        title: "shell".into(),
        subtitle: "tool shell · modifies production".into(),
        live: true,
        allow_always: true,
        call: None,
        preview: Some(serde_json::from_value(preview)?),
    })
}

#[test]
fn approval_preview_preserves_literal_spacing_and_escapes_terminal_controls(
) -> Result<(), Box<dyn std::error::Error>> {
    let source = "  printf 'two  spaces'  \n\n\t\u{1b}[2J\u{202e}end";
    let request = ask(serde_json::json!({"type":"shell","command":source,
        "cwd":"/work","shell":"sh","truncated":false}))?;
    for width in [80, 40, 7] {
        let rows = review_rows(&request, width);
        let out: Vec<_> = rows
            .iter()
            .filter(|(_, style)| *style == Style::token(Token::Code))
            .map(|(text, _)| text.as_str())
            .collect();
        assert_eq!(
            out.concat(),
            "  printf 'two  spaces'  \\t\\u{1b}[2J\\u{202e}end"
        );
        assert!(out.iter().any(|row| row.is_empty()));
        assert!(rows
            .iter()
            .all(|(row, _)| crate::cells::cells(row) <= width));
        assert!(!rows.iter().any(|(row, _)| row.contains('\u{1b}')));
    }
    Ok(())
}

#[test]
fn approval_preview_keeps_patch_end_and_source_truncation_honest(
) -> Result<(), Box<dyn std::error::Error>> {
    let patch = format!(
        "*** Begin Patch\n{}\n*** End Patch",
        "+keep  spaces\n".repeat(60)
    );
    let ask = ask(
        serde_json::json!({"type":"patch","format":"apply-patch","patch":patch,"truncated":true}),
    )?;
    assert!(source_truncated(&ask));
    let rows = review_rows(&ask, 40);
    let text: String = rows.iter().map(|(line, _)| line.as_str()).collect();
    assert!(text.contains("*** End Patch"));
    assert!(text.contains("Source preview truncated; remaining patch unavailable."));
    assert!(rows.iter().all(|(line, _)| crate::cells::cells(line) <= 40));
    Ok(())
}

#[test]
fn approval_preview_handles_absent_and_unavailable_requests(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut ask = ask(serde_json::json!({"type":"unavailable","reason":"legacy request"}))?;
    assert!(!source_truncated(&ask));
    assert!(review_rows(&ask, 40)
        .iter()
        .any(|(line, _)| line.contains("legacy request")));
    ask.preview = None;
    let text: String = review_rows(&ask, 40)
        .into_iter()
        .map(|(line, _)| line)
        .collect();
    assert!(text.contains("Preview unavailable for this request"));
    Ok(())
}

#[test]
fn commands_keep_code_style_and_operations_remain_plain() -> Result<(), Box<dyn std::error::Error>>
{
    let command = ask(serde_json::json!({"type":"shell","command":"  echo tail",
        "cwd":"/work","shell":"sh","truncated":false}))?;
    assert!(review_rows(&command, 40)
        .iter()
        .any(|(line, style)| line == "  echo tail" && *style == Style::token(Token::Code)));
    let operations = ask(serde_json::json!({"type":"patch","format":"operations",
        "patch":"-literal\n+literal","truncated":false}))?;
    let rows = review_rows(&operations, 40);
    for text in ["-literal", "+literal"] {
        assert!(rows
            .iter()
            .any(|(line, style)| line == text && *style == Style::PLAIN));
    }
    Ok(())
}
