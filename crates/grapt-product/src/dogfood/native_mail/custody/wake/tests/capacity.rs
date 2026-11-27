//! Historical bindings must not exhaust capacity needed by a current session.
use super::*;

const IDENTITY: &str = r#"{"scope":"project","project_id":"fixture-project","agent_id":"fixture-recipient","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#;

async fn create(
    application: &Arc<DogfoodApplication>,
    root: &Root,
) -> Result<SessionId, Box<dyn std::error::Error>> {
    let body = serde_json::json!({"location":{"directory":root.0}}).to_string();
    let response = ingress::call(application, "POST", "/api/session", &body).await?;
    assert!(
        response.starts_with(b"HTTP/1.1 200"),
        "session create refused: {}",
        String::from_utf8_lossy(&response)
    );
    let body = response
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .ok_or("HTTP response head")?
        + 4;
    let value: serde_json::Value = serde_json::from_slice(&response[body..])?;
    Ok(value["data"]["id"]
        .as_str()
        .ok_or("session identity")?
        .parse()?)
}

async fn delete(application: &Arc<DogfoodApplication>, session: &SessionId) -> TestResult {
    let response = ingress::call(
        application,
        "DELETE",
        &format!("/api/session/{session}"),
        "",
    )
    .await?;
    assert!(
        response.starts_with(b"HTTP/1.1 204"),
        "session delete refused: {}",
        String::from_utf8_lossy(&response)
    );
    Ok(())
}

async fn bind(
    application: &Arc<DogfoodApplication>,
    session: &SessionId,
    ordinal: usize,
) -> TestResult {
    let response = ingress::call(
        application,
        "PUT",
        &format!("/api/session/{session}/stateengine/capability"),
        r#"{"capability":"fixture-secret"}"#,
    )
    .await?;
    assert!(
        response.starts_with(b"HTTP/1.1 204"),
        "capability binding {ordinal} refused: {}",
        String::from_utf8_lossy(&response)
    );
    Ok(())
}

async fn bind_owner(
    application: &Arc<DogfoodApplication>,
    session: &SessionId,
    ordinal: usize,
) -> TestResult {
    let body = format!("{{\"value\":{IDENTITY}}}");
    let response = ingress::call(
        application,
        "PUT",
        &format!("/api/session/{session}/instructions/entries/stateengine-identity"),
        &body,
    )
    .await?;
    assert!(
        response.starts_with(b"HTTP/1.1 204"),
        "identity update refused: {}",
        String::from_utf8_lossy(&response)
    );
    bind(application, session, ordinal).await?;
    assert!(
        application
            .lock_inbox()
            .native_mail_wakes_available(session)?,
        "current authenticated session lost native owner capacity at binding {ordinal}"
    );
    Ok(())
}

#[tokio::test]
async fn deleted_sessions_release_capability_capacity_for_129_bindings() -> TestResult {
    let (root, application, _driver, first) = fixture(None).await?;
    delete(&application, &first).await?;
    for ordinal in 2..=129 {
        let session = create(&application, &root).await?;
        bind(&application, &session, ordinal).await?;
        delete(&application, &session).await?;
    }
    Ok(())
}

#[tokio::test]
async fn deleted_sessions_release_native_owner_capacity_for_65_bindings() -> TestResult {
    let (root, application, _driver, first) = fixture(None).await?;
    bind_owner(&application, &first, 1).await?;
    delete(&application, &first).await?;
    for ordinal in 2..=65 {
        let session = create(&application, &root).await?;
        bind_owner(&application, &session, ordinal).await?;
        delete(&application, &session).await?;
    }
    Ok(())
}

#[tokio::test]
async fn superseded_sessions_release_native_owner_capacity_for_65_bindings() -> TestResult {
    let (root, application, _driver, mut previous) = fixture(None).await?;
    bind_owner(&application, &previous, 1).await?;
    for ordinal in 2..=65 {
        let session = create(&application, &root).await?;
        bind_owner(&application, &session, ordinal).await?;
        assert!(
            !application
                .lock_inbox()
                .native_mail_wakes_available(&previous)?,
            "superseded session retained delivery authority"
        );
        previous = session;
    }
    Ok(())
}
