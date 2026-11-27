use super::*;

fn post(path: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .body(Body::from(body.to_owned()))
        .unwrap_or_else(|error| panic!("rewind request: {error}"))
}

#[tokio::test]
async fn rw_native_routes_require_authentication_and_closed_typed_bodies() {
    for (endpoint, path, body) in [
        (
            Endpoint::SessionRewindPreview,
            "/api/session/ses_rewind/rewind/preview",
            r#"{"prompts":1}"#,
        ),
        (
            Endpoint::SessionRewind,
            "/api/session/ses_rewind/rewind",
            r#"{"messageID":"msg_selected","tip":7}"#,
        ),
    ] {
        let state = Arc::new(RouteState {
            application: ApplicationBinding::Fixed(never_application()),
        });
        assert_eq!(
            handle(state, endpoint, post(path, body)).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert!(request_for(endpoint, post(path, body)).await.is_ok());
        for refused in [
            "{}",
            r#"{"prompts":null,"messageID":null,"tip":null}"#,
            r#"{"extra":1}"#,
        ] {
            let Err(response) = request_for(endpoint, post(path, refused)).await else {
                panic!("invalid rewind body reached application dispatch");
            };
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        let Err(response) = request_for(endpoint, post(&format!("{path}?extra=1"), body)).await
        else {
            panic!("rewind query reached application dispatch");
        };
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
