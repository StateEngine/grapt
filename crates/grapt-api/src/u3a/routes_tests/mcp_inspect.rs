use super::*;
#[tokio::test]
async fn mcp_inspection_read_preserves_location_and_rejects_unknown_queries() {
    let parsed = request_for(
        Endpoint::McpInspect,
        request("/api/mcp/inspect?location%5Bdirectory%5D=%2Fwork%2Frepo"),
    )
    .await;
    match parsed {
        Ok(ApplicationRequest::McpInspect(query)) => {
            let value = serde_json::to_value(query)
                .unwrap_or_else(|error| panic!("MCP inspect query: {error}"));
            assert_eq!(value["location"]["directory"], "/work/repo");
        }
        other => panic!("MCP inspection lost its location: {other:?}"),
    }
    let Err(response) =
        request_for(Endpoint::McpInspect, request("/api/mcp/inspect?surprise=1")).await
    else {
        panic!("unknown inspection query reached dispatch");
    };
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
#[tokio::test]
async fn mcp_inspection_uses_the_existing_authenticated_boundary() {
    let state = Arc::new(RouteState {
        application: ApplicationBinding::Fixed(never_application()),
    });
    let response = handle(state, Endpoint::McpInspect, request("/api/mcp/inspect")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mcp_uses_the_existing_typed_session_selector() {
    let parsed = request_for(
        Endpoint::McpInspect,
        request("/api/mcp/inspect?session=ses_catalog_scope"),
    )
    .await;
    match parsed {
        Ok(ApplicationRequest::McpInspect(query)) => assert_eq!(
            query
                .session()
                .as_present()
                .map(|id| id.as_inner().as_str()),
            Some("ses_catalog_scope")
        ),
        other => panic!("MCP Session selector was lost: {other:?}"),
    }
    for uri in [
        "/api/mcp/inspect?session=invalid",
        "/api/mcp/inspect?session=ses_one&session=ses_two",
    ] {
        assert!(request_for(Endpoint::McpInspect, request(uri))
            .await
            .is_err());
    }
    assert!(request_for(
        Endpoint::ProjectCurrent,
        request("/api/project/current?sessionID=ses_catalog_scope")
    )
    .await
    .is_err());
}
