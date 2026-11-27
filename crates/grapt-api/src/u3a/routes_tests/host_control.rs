use super::*;
use crate::u3a::HostControlRequest;

#[tokio::test]
async fn host_control_http_requests_keep_typed_keys_and_closed_notice_body() -> Result<(), String> {
    let parsed = request_for(
        Endpoint::HostInput,
        request("/api/session/ses_host/input/msg_initial"),
    )
    .await
    .map_err(|response| format!("input readback refused: {}", response.status()))?;
    let ApplicationRequest::HostControl(HostControlRequest::Input { session, input }) = parsed
    else {
        return Err("input readback lost its typed request".into());
    };
    assert_eq!(session.as_inner().as_str(), "ses_host");
    assert_eq!(input.as_inner().as_str(), "msg_initial");
    let notice_path = "/api/session/ses_host/stateengine/notice";
    assert!(matches!(
        request_for(Endpoint::HostNoticeBinding, request(notice_path)).await,
        Ok(ApplicationRequest::HostControl(
            HostControlRequest::Binding { .. }
        ))
    ));
    let mut document = serde_json::json!({
        "id":"msg_notice", "targetID":"evt_target", "binding":"a".repeat(64),
        "messages":["mail-1"], "incarnation":"incarnation-test",
        "serviceInstance":"instance-test",
        "generation":7, "registrationVersion":"version-test"
    });
    let notice_request = |document: &serde_json::Value| {
        Request::builder()
            .method("POST")
            .uri(notice_path)
            .body(Body::from(document.to_string()))
            .map_err(|error| error.to_string())
    };
    let parsed = request_for(Endpoint::HostNotice, notice_request(&document)?)
        .await
        .map_err(|response| format!("notice refused: {}", response.status()))?;
    let ApplicationRequest::HostControl(HostControlRequest::Notice {
        session,
        request: notice,
    }) = parsed
    else {
        return Err("notice lost its typed request".into());
    };
    assert_eq!(session.as_inner().as_str(), "ses_host");
    assert_eq!(notice.id().get().as_inner().as_str(), "msg_notice");
    assert_eq!(notice.target_id().get().as_inner().as_str(), "evt_target");
    assert_eq!(notice.messages().get().as_slice()[0].as_str(), "mail-1");
    document["extra"] = serde_json::json!(true);
    assert!(
        request_for(Endpoint::HostNotice, notice_request(&document)?)
            .await
            .is_err()
    );
    for path in [
        "/api/session/ses_host/input/not-a-message",
        "/api/session/ses_host/input/msg_initial?extra=1",
    ] {
        assert!(request_for(Endpoint::HostInput, request(path))
            .await
            .is_err());
    }
    Ok(())
}
