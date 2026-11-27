use super::*;
fn post(path: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .body(Body::from(body.to_owned()))
        .unwrap_or_else(|error| panic!("undo request: {error}"))
}
#[tokio::test]
async fn pu_file_undo_requires_authentication_and_closed_exact_target_bodies() {
    for (endpoint, path, body) in [
        (
            Endpoint::SessionUndoPreview,
            "/api/session/ses_undo/undo/preview",
            "{}",
        ),
        (
            Endpoint::SessionUndo,
            "/api/session/ses_undo/undo",
            r#"{"inputID":"msg_selected","target":"checkpoint"}"#,
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
            "[]",
            "null",
            "true",
            r#"{"extra":1}"#,
            r#"{"inputID":null,"target":null}"#,
            r#"{"target":"checkpoint"}"#,
        ] {
            let Err(response) = request_for(endpoint, post(path, refused)).await else {
                panic!("invalid file undo reached application dispatch");
            };
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        let Err(response) = request_for(endpoint, post(&format!("{path}?extra=1"), body)).await
        else {
            panic!("undo query reached dispatch");
        };
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
