//! Local inspection stays separate from prompt submission and scrolling.
use super::*;

#[test]
fn context_inspects_captured_instructions_without_submission_and_keeps_failed_drafts() -> TestResult
{
    let mut client = prepared(true)?;
    let session = client.selected().cloned().ok_or("session")?;
    let uncaptured = run(&mut client, "/context")?;
    assert!(uncaptured.consume);
    let Some(View::Text(lines)) = uncaptured.view else {
        return Err("missing instruction result".into());
    };
    assert!(lines
        .iter()
        .any(|line| line.contains("not been captured yet")));
    client.peer_mut().instruction_response = Some(WireResponse {
        status: 200,
        body: serde_json::to_vec(&serde_json::json!({
            "captured":true,
            "keys":["core/instructions","private/seat"],
            "ambient":"# /repo/AGENTS.md\nPreserve existing work.\n"
        }))?,
    });
    let captured = run(&mut client, "/context")?;
    assert!(captured.consume && captured.session.as_ref() == Some(&session));
    let Some(View::Text(lines)) = captured.view else {
        return Err("missing captured result".into());
    };
    assert!(lines.iter().any(|line| line.contains("private/seat")));
    assert!(lines.iter().any(|line| line == "# /repo/AGENTS.md"));
    assert!(lines.iter().any(|line| line == "Preserve existing work."));
    for response in [
        WireResponse {
            status: 503,
            body: Vec::new(),
        },
        WireResponse {
            status: 200,
            body: b"{}".to_vec(),
        },
    ] {
        client.peer_mut().instruction_response = Some(response);
        let failed = run(&mut client, "/context")?;
        assert!(!failed.consume && failed.replacement.is_none());
        assert_eq!(failed.text, "/context");
        assert!(matches!(failed.view, Some(View::Notice(_))));
    }
    let calls = client.peer_mut().calls.len();
    assert!(!run(&mut client, "/context extra")?.consume);
    assert_eq!(client.peer_mut().calls.len(), calls);
    assert!(!submits_prompt("/context"));
    assert!(crate::slash::builtin("context"));
    assert!(client.peer_mut().prompts.is_empty());
    Ok(())
}

#[test]
fn only_prompts_and_valid_init_leave_the_reading_window() {
    for text in [
        "/status",
        "/help",
        "/model",
        "/compact",
        "/reconcile",
        "/new",
        "/resume saved",
        "/diff",
        "/mcp verbose",
        "/init invalid",
    ] {
        assert!(!submits_prompt(text), "{text}");
    }
    for text in ["ordinary task", "/modelish", "/status extra", " /init "] {
        assert!(submits_prompt(text), "{text}");
    }
}
