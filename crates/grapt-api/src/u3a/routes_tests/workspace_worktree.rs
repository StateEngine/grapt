use super::*;

fn post(uri: &str, raw: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .body(Body::from(raw.to_owned()))
        .unwrap_or_else(|error| panic!("worktree request: {error}"))
}

#[tokio::test]
async fn worktree_actions_keep_location_and_reject_malformed_requests() {
    let uri = "/api/workspace/worktree?session=ses_worktree&location%5Bdirectory%5D=%2Fwork%2Frepo";
    for raw in [
        r#"{"action":"create","destination":"../checkout"}"#,
        r#"{"action":"create","destination":"../checkout","setup_files":["local.env"]}"#,
        r#"{"action":"merge","directory":"../checkout"}"#,
        r#"{"action":"remove","directory":"../checkout"}"#,
    ] {
        let parsed = request_for(Endpoint::WorkspaceWorktree, post(uri, raw))
            .await
            .unwrap_or_else(|_| panic!("valid action refused: {raw}"));
        let ApplicationRequest::WorkspaceWorktree { query, request } = parsed else {
            panic!("wrong application operation");
        };
        let query = serde_json::to_value(query).unwrap_or_else(|error| panic!("query: {error}"));
        assert_eq!(query["session"], "ses_worktree");
        assert_eq!(query["location"]["directory"], "/work/repo");
        if let crate::u3a::WorkspaceWorktreeRequestDto::Create { setup_files, .. } = request {
            assert_eq!(setup_files.len(), usize::from(raw.contains("setup_files")));
        }
    }
    let oversized = format!(
        r#"{{"action":"create","destination":"{}"}}"#,
        "x".repeat(4097)
    );
    for raw in [
        "",
        "null",
        r#"{"action":"unknown","directory":"x"}"#,
        r#"{"action":"create","destination":"x","setup_files":null}"#,
        r#"{"action":"merge","directory":"x","extra":true}"#,
        r#"{"action":"remove","directory":"x","destination":"y"}"#,
        r#"{"action":"merge","directory":"x","directory":"y"}"#,
        oversized.as_str(),
    ] {
        assert!(
            request_for(Endpoint::WorkspaceWorktree, post(uri, raw))
                .await
                .is_err(),
            "accepted {raw}"
        );
    }
    let invalid_query = post("/api/workspace/worktree?unknown=1", "{}");
    assert!(request_for(Endpoint::WorkspaceWorktree, invalid_query)
        .await
        .is_err());
}

#[tokio::test]
async fn worktree_mutations_require_authentication_and_are_absent_in_ephemeral_routes() {
    let state = Arc::new(RouteState {
        application: ApplicationBinding::Fixed(never_application()),
    });
    let request = post("/api/workspace/worktree", "{}");
    let response = handle(state, Endpoint::WorkspaceWorktree, request).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let mut builder = ApiRouterBuilder::new(
        MaxRouterModules::try_new(DOGFOOD_NON_SSE_ROUTE_COUNT)
            .unwrap_or_else(|error| panic!("route bound: {error}")),
    );
    register_ephemeral_dogfood_routes(&mut builder, never_application())
        .unwrap_or_else(|error| panic!("ephemeral routes: {error}"));
    let routes = builder.seal().route_set();
    assert!(!routes
        .iter()
        .any(|route| route.contains("/api/workspace/worktree")));
}

#[tokio::test]
async fn worktree_outcomes_require_closed_effect_and_preserve_it_in_http_200_replies() {
    let mut value = serde_json::json!({
        "location":{"directory":"/source","project":{"id":"project","directory":"/source"}},
        "data":{"directory":"/source","message":"Inspect source and checkout before retrying"}
    });
    for effect in ["completed", "not_started", "may_have_changed"] {
        value["data"]["effect"] = serde_json::json!(effect);
        let dto: crate::u3a::WorkspaceWorktreeResponseDto = serde_json::from_value(value.clone())
            .unwrap_or_else(|error| panic!("worktree outcome: {error}"));
        let response = json(Endpoint::WorkspaceWorktree.success_status(), &dto);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_json(response, "worktree outcome").await["data"]["effect"],
            effect
        );
    }
    for effect in [serde_json::Value::Null, serde_json::json!("unknown")] {
        value["data"]["effect"] = effect;
        assert!(
            serde_json::from_value::<crate::u3a::WorkspaceWorktreeResponseDto>(value.clone())
                .is_err()
        );
    }
    value["data"]
        .as_object_mut()
        .unwrap_or_else(|| panic!("outcome data"))
        .remove("effect");
    assert!(serde_json::from_value::<crate::u3a::WorkspaceWorktreeResponseDto>(value).is_err());
}
