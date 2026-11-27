use super::*;

#[test]
fn inline_model_and_effort_use_the_existing_atomic_selection() -> TestResult {
    for session in [false, true] {
        let mut client = prepared(session)?;
        let original = client.model().cloned().ok_or("selected model")?;
        let model = format!(
            "{}/{}",
            original.provider_id.spelled(),
            original.id.spelled()
        );
        let high = run(&mut client, &format!("/model {model} high"))?;
        assert!(high.consume && high.notice.contains("effort: high"));
        let low = run(&mut client, "/effort low")?;
        assert!(low.consume && low.notice.contains("effort: low"));
        assert_eq!(
            client
                .model()
                .and_then(|m| m.variant.as_ref())
                .map(Spelled::spelled),
            Some("low")
        );
        let default = run(&mut client, "/effort default")?;
        assert!(default.consume);
        assert!(client.model().is_some_and(|m| m.variant.is_none()));
        assert!(client.peer_mut().prompts.is_empty());
        assert_eq!(
            client
                .peer_mut()
                .calls
                .contains(&Operation::SessionModelSelect),
            session
        );
    }
    Ok(())
}

#[test]
fn effort_picker_and_status_never_submit_a_model_request() -> TestResult {
    let mut client = prepared(true)?;
    let before = client.peer_mut().calls.len();
    assert!(matches!(
        run(&mut client, "/effort")?.view,
        Some(View::Models)
    ));
    let status = run(&mut client, "/effort status")?;
    let Some(View::Text(rows)) = status.view else {
        return Err("effort status pane".into());
    };
    assert!(status.consume && rows.join("\n").contains("Effort: high"));
    assert_eq!(client.peer_mut().calls.len(), before);
    assert!(client.peer_mut().prompts.is_empty());
    Ok(())
}

#[test]
fn invalid_model_and_effort_preserve_the_selection_and_command() -> TestResult {
    let mut client = prepared(true)?;
    let before = client.model().cloned();
    let requests = client.peer_mut().calls.len();
    for text in [
        "/model missing",
        "/model nowhere/model high",
        "/model a b c",
        "/effort missing",
        "/effort high unexpected",
        "/effort status extra",
    ] {
        let done = run(&mut client, text)?;
        assert!(!done.consume && done.replacement.is_none(), "{text}");
        assert_eq!(client.model(), before.as_ref(), "{text}");
    }
    assert_eq!(client.peer_mut().calls.len(), requests);
    assert!(client.peer_mut().prompts.is_empty());
    Ok(())
}
