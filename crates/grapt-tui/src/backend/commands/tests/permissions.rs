use super::*;

#[test]
fn permission_commands_use_server_selection_and_never_submit_prompts() -> TestResult {
    for session in [false, true] {
        let mut client = if session {
            prepared(true)?
        } else {
            fresh_draft()?
        };
        let shown = run(&mut client, "/permissions")?;
        assert!(shown.consume && client.selected().is_some());
        let Some(View::Grants(snapshot)) = shown.view else {
            return Err("permissions pane".into());
        };
        assert!(snapshot.policy.contains("effective:"));
        for name in ["inspect", "author", "maintainer"] {
            let done = run(&mut client, &format!("/permissions {name}"))?;
            assert!(done.consume);
            assert!(client
                .preset_line()
                .is_some_and(|line| line.contains(&format!("Preset: {name}"))));
        }
        assert_eq!(
            client
                .peer_mut()
                .calls
                .iter()
                .filter(|op| **op == Operation::SessionPresetSelect)
                .count(),
            3
        );
        assert!(client.peer_mut().prompts.is_empty());
    }
    Ok(())
}

#[test]
fn invalid_or_unconfirmed_permissions_do_not_create_sessions_or_issue_writes() -> TestResult {
    let mut client = fresh_draft()?;
    let requests = client.peer_mut().calls.len();
    let sessions = client.peer_mut().inner.session_count();
    for text in [
        "/permissions missing",
        "/permissions author extra",
        "/permissions bypass",
        "/permissions bypass nope",
        "/permissions bypass confirm extra",
    ] {
        let done = run(&mut client, text)?;
        assert!(!done.consume && done.replacement.is_none(), "{text}");
        assert!(matches!(done.view, Some(View::Notice(_))), "{text}");
        assert_eq!(client.peer_mut().calls.len(), requests, "{text}");
        assert_eq!(client.peer_mut().inner.session_count(), sessions, "{text}");
    }
    assert!(client.peer_mut().prompts.is_empty());
    Ok(())
}

#[test]
fn only_explicit_bypass_confirmation_selects_and_displays_bypass() -> TestResult {
    let mut client = fresh_draft()?;
    let done = run(&mut client, "/permissions bypass confirm")?;
    assert!(done.consume && client.selected().is_some());
    let Some(View::Text(rows)) = done.view else {
        return Err("bypass pane".into());
    };
    assert!(rows.join("\n").contains("Bypass permissions on"));
    assert_eq!(
        client
            .peer_mut()
            .calls
            .iter()
            .filter(|op| **op == Operation::SessionPresetSelect)
            .count(),
        1
    );
    assert!(client.peer_mut().prompts.is_empty());
    Ok(())
}
