use super::*;

#[test]
fn lost_tool_reconciliation_is_a_local_command_not_another_prompt() -> TestResult {
    let mut client = prepared(true)?;
    let done = run(&mut client, " /reconcile ")?;
    assert!(done.consume);
    assert_eq!(done.notice, "no lost tool runs to reconcile");
    assert!(!submits_prompt("/reconcile"));
    assert_eq!(
        client
            .peer_mut()
            .calls
            .iter()
            .filter(|operation| **operation == Operation::SessionReconcile)
            .count(),
        1
    );
    assert!(
        client.peer_mut().prompts.is_empty(),
        "recovery must never become a model task"
    );
    Ok(())
}

#[test]
fn reconciliation_reports_unknown_effects_without_claiming_completion() -> TestResult {
    let mut client = prepared(true)?;
    for (count, notice) in [
        (
            1,
            "recorded 1 lost tool run as unknown; check its effect before continuing",
        ),
        (
            3,
            "recorded 3 lost tool runs as unknown; check their effects before continuing",
        ),
    ] {
        client.peer_mut().reconciled = count;
        let done = run(&mut client, "/reconcile")?;
        assert!(done.consume);
        assert_eq!(done.notice, notice);
        assert!(done.replacement.is_none());
        assert_eq!(done.session.as_ref(), client.selected());
        let Some(View::Text(rows)) = done.view else {
            return Err("recovery result must survive later connection status updates".into());
        };
        assert!(rows.iter().any(|row| row == notice));
    }
    assert!(client.peer_mut().prompts.is_empty());
    Ok(())
}

#[test]
fn reconciliation_refusal_keeps_the_draft_and_requires_explicit_retry() -> TestResult {
    let mut client = prepared(true)?;
    client.peer_mut().refuse_reconcile = true;
    let done = run(&mut client, "/reconcile")?;
    assert!(!done.consume);
    assert_eq!(done.text, "/reconcile");
    assert!(done.notice.contains("503"));
    assert!(matches!(done.view, Some(View::Text(_))));
    assert_eq!(
        client
            .peer_mut()
            .calls
            .iter()
            .filter(|op| **op == Operation::SessionReconcile)
            .count(),
        1
    );
    client.peer_mut().refuse_reconcile = false;
    assert!(run(&mut client, "/reconcile")?.consume);
    assert_eq!(
        client
            .peer_mut()
            .calls
            .iter()
            .filter(|op| **op == Operation::SessionReconcile)
            .count(),
        2
    );
    assert!(client.peer_mut().prompts.is_empty());
    Ok(())
}

#[test]
fn reconciliation_needs_a_session_and_late_completion_keeps_the_new_draft() -> TestResult {
    let mut client = prepared(false)?;
    let done = run(&mut client, "/reconcile")?;
    assert!(!done.consume);
    assert!(!client
        .peer_mut()
        .calls
        .contains(&Operation::SessionReconcile));
    client.create_session()?;
    let mut done = run(&mut client, "/reconcile")?;
    client.create_session()?;
    assert!(!done.keep_for(client.selected()));
    assert!(!done.consume);
    for text in ["/reconcileish", "/reconcile extra"] {
        assert!(!run(&mut client, text)?.consume);
    }
    Ok(())
}
