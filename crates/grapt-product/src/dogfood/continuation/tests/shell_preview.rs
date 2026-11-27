use super::*;

#[test]
fn large_shell_context_is_stable_after_append_and_reopen_while_history_stays_exact(
) -> Result<(), String> {
    let mut fixture = Fixture::new("shell-preview-history")?;
    let bytes = format!(
        "HEAD{}TAIL\nCommand exited with code 0.",
        "campaign status row\n".repeat(27000)
    )
    .into_bytes();
    fixture
        .record_exchange_call(
            (true, false, false),
            "shell",
            "shell",
            &bytes,
            r#"{"command":"fixture campaign status"}"#,
        )?
        .ok_or("missing output")?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let before = continuation.request_input("use the tool", true)?;
    let preview = before
        .iter()
        .find_map(|item| match item {
            CodexInputItem::ToolOutput { output, .. } => Some(output.as_str().to_owned()),
            _ => None,
        })
        .ok_or("missing shell output")?;
    assert!(preview.len() <= 32 * 1024);
    assert!(preview.starts_with("HEAD"));
    assert!(preview.ends_with("Command exited with code 0."));
    let call = ToolCallId::new("call_d2a_read").ok_or("call")?;
    let projection = tool_projection(
        fixture.inbox()?,
        fixture.artifacts()?,
        &fixture.session,
        &call,
    )?;
    assert_eq!(projection.output.as_bytes(), bytes);
    let mut recovered = String::new();
    let mut offset = 0;
    for _ in 0..16 {
        let page = history(
            fixture.inbox()?,
            fixture.artifacts()?,
            &fixture.session,
            Some(&call),
            None,
            Some(offset),
        )?;
        let page: serde_json::Value = serde_json::from_slice(&page).map_err(|e| e.to_string())?;
        recovered.push_str(page["content"].as_str().ok_or("history content")?);
        match page["nextOffset"].as_u64() {
            Some(next) => offset = next,
            None => break,
        }
    }
    assert_eq!(recovered.as_bytes(), bytes);
    let next_id: MessageId = "msg_shell_preview_next"
        .parse()
        .map_err(|e: grapt_types::IdError| e.to_string())?;
    promote_test_input(fixture.inbox()?, &fixture.session, &next_id, "continue")?;
    let next = fixture.continuation(next_id.clone())?;
    let after = next.request_input("continue", false)?;
    assert!(
        after.starts_with(&before),
        "appending a user input changed the old shell prefix"
    );
    drop(next);
    drop(continuation);
    fixture.close();
    let reopened = fixture.reopen(next_id)?;
    assert_eq!(reopened.request_input("continue", false)?, after);
    Ok(())
}
