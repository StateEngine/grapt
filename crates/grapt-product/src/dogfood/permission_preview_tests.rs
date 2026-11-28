use super::*;

#[test]
fn permission_preview_survives_polling_and_store_reopen() -> TestResult {
    let root = Root::new()?;
    let app = application(&root, true)?;
    let session = session(&app, &root)?;
    let client = AuthenticatedClientId::new("client_preview").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "subject_preview",
        client: &client,
    };
    let preview = serde_json::json!({"type":"shell","command":"pwd", "cwd":"/repo", "shell":"/bin/sh", "truncated":false});
    let metadata = serde_json::json!({"tool":"shell", "request":{"approvalPreview":preview}});
    ask(&app, &owner, &session, 980, Some(&metadata))?;
    let check = |app: &ServerApplication| -> TestResult {
        let page =
            polling_page(app, &owner, Some(&session)).map_err(|error| format!("{error:?}"))?;
        assert_eq!(page.requests.len(), 1);
        let wire = crate::dogfood::permission::permission_json(&page.requests[0]);
        assert_eq!(wire["preview"], preview);
        serde_json::from_value::<grapt_api::u3a::PermissionRequestStateDto>(wire)?;
        Ok(())
    };
    check(&app)?;
    drop(app);
    let reopened = application(&root, true)?;
    check(&reopened)?;
    Ok(())
}

#[test]
fn malformed_legacy_preview_is_explicitly_unavailable_without_hiding_the_ask() {
    let mut value = serde_json::json!({"metadata":{"request":{"approvalPreview":{
        "type":"shell", "command":"missing cwd and shell"}}}});
    crate::dogfood::permission::include_preview(&mut value);
    assert_eq!(value["preview"]["type"], "unavailable");
    let mut large = serde_json::json!({"metadata":{"request":{"approvalPreview":{
        "type":"shell", "command":"a".repeat(8192), "cwd":"/repo", "shell":"/bin/sh", "truncated":false}}}});
    crate::dogfood::permission::include_preview(&mut large);
    assert_eq!(large["preview"]["type"], "unavailable");
}
