use super::*;
#[tokio::test]
async fn command_inspection_read_preserves_location_and_rejects_unknown_queries() {
    let parsed = request_for(
        Endpoint::CommandInspect,
        request("/api/command/inspect?location%5Bdirectory%5D=%2Fwork%2Frepo"),
    )
    .await;
    match parsed {
        Ok(ApplicationRequest::CommandInspect(query)) => {
            let value = serde_json::to_value(query)
                .unwrap_or_else(|error| panic!("command inspect query: {error}"));
            assert_eq!(value["location"]["directory"], "/work/repo");
        }
        other => panic!("command inspection lost its location: {other:?}"),
    }
    let Err(response) = request_for(
        Endpoint::CommandInspect,
        request("/api/command/inspect?surprise=1"),
    )
    .await
    else {
        panic!("unknown inspection query reached dispatch");
    };
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
#[tokio::test]
async fn command_inspection_uses_the_existing_authenticated_boundary() {
    let state = Arc::new(RouteState {
        application: ApplicationBinding::Fixed(never_application()),
    });
    let response = handle(
        state,
        Endpoint::CommandInspect,
        request("/api/command/inspect"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
