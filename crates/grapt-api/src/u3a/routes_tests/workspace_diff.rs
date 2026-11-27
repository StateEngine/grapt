use super::*;

#[tokio::test]
async fn diff_read_keeps_the_requested_location_and_rejects_unknown_queries() {
    let parsed = request_for(
        Endpoint::WorkspaceDiff,
        request("/api/workspace/diff?location%5Bdirectory%5D=%2Fwork%2Frepo"),
    )
    .await;
    match parsed {
        Ok(ApplicationRequest::WorkspaceDiff(query)) => {
            let value =
                serde_json::to_value(query).unwrap_or_else(|error| panic!("diff query: {error}"));
            assert_eq!(value["location"]["directory"], "/work/repo");
        }
        other => panic!("workspace diff lost its location: {other:?}"),
    }
    let Err(response) = request_for(
        Endpoint::WorkspaceDiff,
        request("/api/workspace/diff?surprise=1"),
    )
    .await
    else {
        panic!("unknown diff query reached application dispatch");
    };
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(Endpoint::WorkspaceDiff.success_status(), StatusCode::OK);
}

#[tokio::test]
async fn diff_read_uses_the_existing_authenticated_boundary() {
    let state = Arc::new(RouteState {
        application: ApplicationBinding::Fixed(never_application()),
    });
    let response = handle(
        state,
        Endpoint::WorkspaceDiff,
        request("/api/workspace/diff"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
