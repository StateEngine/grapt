use super::super::{
    AgentListResponseDto, AgentLocationQueryDto, ApplicationFuture, ConflictErrorDto,
    ConflictErrorDtoTag, DtoString, FormAlreadySettledErrorDto, FormAlreadySettledErrorDtoTag,
    FormInvalidAnswerErrorDto, FormInvalidAnswerErrorDtoTag, FormNotFoundErrorDto,
    FormNotFoundErrorDtoTag, InvalidCursorErrorDto, InvalidCursorErrorDtoTag, MessageListQueryDto,
    ModelDefaultLocationQueryDto, ModelDefaultResponseDto, ModelListResponseDto,
    ModelLocationQueryDto, NoContent, PermissionCreateResponseDto, PermissionGlobalListResponseDto,
    PermissionListResponseDto, PermissionLocationQueryDto, PermissionResponseDto,
    ProjectCurrentDto, ProjectCurrentLocationQueryDto, ProjectListResponseDto, PromptRequestDto,
    PromptResponseDto, ProviderListResponseDto, ProviderLocationQueryDto,
    SessionActiveApplicationError, SessionActiveResponseDto, SessionListQueryDto,
    SessionMessagesResponseDto, SessionNotFoundErrorDto, SessionNotFoundErrorDtoTag,
    SessionResponseDto, SessionsResponseDto, UnknownErrorDto, UnknownErrorDtoTag,
};
use super::*;
use crate::u3a::limits::MAX_REQUEST_DTO_BYTES;
use crate::MaxRouterModules;
use axum::body::to_bytes;
use axum::http::header::CONTENT_TYPE;

#[path = "routes_tests/route_census.rs"]
mod route_census;

#[path = "routes_tests/host_control.rs"]
mod host_control;
#[path = "routes_tests/preset.rs"]
mod preset;
#[path = "routes_tests/rewind.rs"]
mod rewind;
#[path = "routes_tests/undo.rs"]
mod undo;

#[path = "routes_tests/application.rs"]
mod application;
use application::never_application;

fn text(value: &'static str) -> DtoString {
    match DtoString::try_new(value) {
        Ok(value) => value,
        Err(error) => panic!("error fixture text was refused: {error}"),
    }
}

fn invalid() -> InvalidRequestErrorDto {
    InvalidRequestErrorDto::new(
        Required::new(InvalidRequestErrorDtoTag::InvalidRequestError),
        Required::new(text("invalid")),
        OptionalNullable::absent(),
        OptionalNullable::absent(),
    )
}

fn cursor() -> InvalidCursorErrorDto {
    InvalidCursorErrorDto::new(
        Required::new(InvalidCursorErrorDtoTag::InvalidCursorError),
        Required::new(text("cursor")),
    )
}

fn not_found() -> SessionNotFoundErrorDto {
    let Some(session_id) = WireId::parse(String::from("ses_missing")) else {
        panic!("session-not-found fixture identity was invalid");
    };
    SessionNotFoundErrorDto::new(
        Required::new(SessionNotFoundErrorDtoTag::SessionNotFoundError),
        Required::new(session_id),
        Required::new(text("missing")),
    )
}

fn request(uri: &str) -> Request<Body> {
    match Request::builder().uri(uri).body(Body::empty()) {
        Ok(request) => request,
        Err(error) => panic!("route fixture request failed: {error}"),
    }
}

async fn response_json(response: Response, label: &str) -> serde_json::Value {
    let bytes = match to_bytes(response.into_body(), MAX_REQUEST_DTO_BYTES).await {
        Ok(bytes) => bytes,
        Err(error) => panic!("{label} response body was unreadable: {error}"),
    };
    match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(error) => panic!("{label} response body was not JSON: {error}; bytes={bytes:?}"),
    }
}

#[tokio::test]
async fn every_required_write_rejects_transport_absence_before_application_dispatch() {
    for endpoint in [
        Endpoint::McpAdd,
        Endpoint::McpAuth,
        Endpoint::SessionSubagentModelSet,
        Endpoint::SessionCreate,
        Endpoint::SessionRename,
        Endpoint::SessionAgentSelect,
        Endpoint::SessionModelSelect,
        Endpoint::SessionPresetSelect,
        Endpoint::SessionPrompt,
        Endpoint::SessionInterrupt,
        Endpoint::SessionPermissionCreate,
        Endpoint::SessionPermissionReply,
        Endpoint::SessionFormCreate,
        Endpoint::SessionFormReply,
        Endpoint::HostNotice,
    ] {
        let uri = match endpoint {
            Endpoint::McpAdd => "/api/mcp/docs",
            Endpoint::McpAuth => "/api/mcp/docs/auth",
            Endpoint::SessionSubagentModelSet => "/api/session/ses_body/subagent-model",
            Endpoint::SessionCreate => "/api/session",
            Endpoint::SessionRename => "/api/session/ses_body/rename",
            Endpoint::SessionAgentSelect => "/api/session/ses_body/agent",
            Endpoint::SessionModelSelect => "/api/session/ses_body/model",
            Endpoint::SessionPresetSelect => "/api/session/ses_body/preset",
            Endpoint::SessionPrompt => "/api/session/ses_body/prompt",
            Endpoint::SessionInterrupt => "/api/session/ses_body/interrupt",
            Endpoint::SessionPermissionCreate => "/api/session/ses_body/permission",
            Endpoint::SessionPermissionReply => "/api/session/ses_body/permission/per_body/reply",
            Endpoint::SessionFormCreate => "/api/session/ses_body/form",
            Endpoint::SessionFormReply => "/api/session/ses_body/form/frm_body/reply",
            Endpoint::HostNotice => "/api/session/ses_body/stateengine/notice",
            _ => panic!("body witness contains a bodyless endpoint"),
        };
        let response = request_for(endpoint, request(uri)).await;
        let Err(response) = response else {
            panic!("bodyless {endpoint:?} reached application request construction");
        };
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = match to_bytes(response.into_body(), MAX_REQUEST_DTO_BYTES).await {
            Ok(bytes) => bytes,
            Err(error) => panic!("bodyless {endpoint:?} response was unreadable: {error}"),
        };
        let required = b"request body is required";
        assert!(
            bytes
                .windows(required.len())
                .any(|window| window == required),
            "bodyless {endpoint:?} did not name the required-body refusal"
        );
    }
}

#[test]
fn each_route_specific_error_union_emits_only_its_frozen_statuses() {
    assert_eq!(
        catalog_error_response(CatalogApplicationError::InvalidRequest(invalid())).status(),
        StatusCode::BAD_REQUEST
    );
    let unavailable = match ApplicationUnavailable::try_new("provider") {
        Ok(value) => value,
        Err(error) => panic!("unavailable fixture was refused: {error:?}"),
    };
    assert_eq!(
        catalog_error_response(CatalogApplicationError::Unavailable(unavailable)).status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        session_list_error_response(SessionListApplicationError::InvalidRequest(invalid()))
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        session_list_error_response(SessionListApplicationError::InvalidCursor(cursor())).status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        session_lookup_error_response(SessionLookupApplicationError::InvalidRequest(invalid()))
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        session_lookup_error_response(SessionLookupApplicationError::SessionNotFound(not_found()))
            .status(),
        StatusCode::NOT_FOUND
    );
    // A busy reader is a transient capacity wait, not a bad request: the active
    // session read must answer exactly as the other history reads do, with a
    // retryable 503 and Retry-After. A 400 here sends a client off to fail.
    let busy = session_active_error_response(SessionActiveApplicationError::Busy);
    assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        busy.headers().get(axum::http::header::RETRY_AFTER),
        Some(&axum::http::HeaderValue::from_static("1")),
        "a busy read must tell the caller when to retry"
    );
    assert_eq!(
        session_active_error_response(SessionActiveApplicationError::InvalidRequest(invalid()))
            .status(),
        StatusCode::BAD_REQUEST,
        "a genuinely unacceptable request stays a 400"
    );
    assert_eq!(
        session_messages_error_response(SessionMessagesApplicationError::InvalidCursor(cursor()))
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        session_messages_error_response(SessionMessagesApplicationError::SessionNotFound(
            not_found()
        ))
        .status(),
        StatusCode::NOT_FOUND
    );
    let unknown = UnknownErrorDto::new(
        Required::new(UnknownErrorDtoTag::UnknownError),
        Required::new(text("unknown")),
        OptionalNullable::absent(),
    );
    assert_eq!(
        session_messages_error_response(SessionMessagesApplicationError::Unknown(unknown)).status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    let conflict = ConflictErrorDto::new(
        Required::new(ConflictErrorDtoTag::ConflictError),
        Required::new(text("conflict")),
        OptionalNullable::absent(),
    );
    assert_eq!(
        session_prompt_error_response(SessionPromptApplicationError::Conflict(conflict)).status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        session_prompt_error_response(SessionPromptApplicationError::SessionNotFound(not_found()))
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn success_and_every_error_family_emit_populated_parseable_json_objects() {
    let Some(project_id) = WireId::parse(String::from("project")) else {
        panic!("success fixture project identity was invalid");
    };
    let success = ProjectCurrentDto::new(Required::new(project_id), Required::new(text("work")));
    let success = response_json(json(StatusCode::OK, &success), "success").await;
    assert_eq!(success.get("id"), Some(&serde_json::json!("project")));
    assert_eq!(success.get("directory"), Some(&serde_json::json!("work")));

    let unauthorized = response_json(unauthorized_response(), "unauthorized").await;
    assert_eq!(
        unauthorized.get("_tag"),
        Some(&serde_json::json!("UnauthorizedError"))
    );
    assert_eq!(
        unauthorized.get("message"),
        Some(&serde_json::json!("authentication required"))
    );

    let invalid = InvalidRequestErrorDto::new(
        Required::new(InvalidRequestErrorDtoTag::InvalidRequestError),
        Required::new(text("invalid")),
        OptionalNullable::value(text("validation")),
        OptionalNullable::value(text("workspace")),
    );
    let invalid = response_json(json(StatusCode::BAD_REQUEST, &invalid), "invalid").await;
    assert_eq!(
        invalid.get("_tag"),
        Some(&serde_json::json!("InvalidRequestError"))
    );
    assert_eq!(invalid.get("kind"), Some(&serde_json::json!("validation")));
    assert_eq!(invalid.get("field"), Some(&serde_json::json!("workspace")));

    let unavailable = match ApplicationUnavailable::try_new("provider") {
        Ok(value) => value,
        Err(error) => panic!("unavailable fixture was refused: {error:?}"),
    };
    let unavailable = response_json(unavailable_response(Ok(unavailable)), "unavailable").await;
    assert_eq!(
        unavailable.get("_tag"),
        Some(&serde_json::json!("ServiceUnavailableError"))
    );
    assert_eq!(
        unavailable.get("service"),
        Some(&serde_json::json!("provider"))
    );

    let cursor = response_json(
        session_list_error_response(SessionListApplicationError::InvalidCursor(cursor())),
        "cursor",
    )
    .await;
    assert_eq!(
        cursor.get("_tag"),
        Some(&serde_json::json!("InvalidCursorError"))
    );
    assert_eq!(cursor.get("message"), Some(&serde_json::json!("cursor")));

    let not_found = response_json(
        session_lookup_error_response(SessionLookupApplicationError::SessionNotFound(not_found())),
        "not-found",
    )
    .await;
    assert_eq!(
        not_found.get("_tag"),
        Some(&serde_json::json!("SessionNotFoundError"))
    );
    assert_eq!(
        not_found.get("sessionID"),
        Some(&serde_json::json!("ses_missing"))
    );

    let unknown = UnknownErrorDto::new(
        Required::new(UnknownErrorDtoTag::UnknownError),
        Required::new(text("unknown")),
        OptionalNullable::value(text("operation")),
    );
    let unknown = response_json(
        session_messages_error_response(SessionMessagesApplicationError::Unknown(unknown)),
        "unknown",
    )
    .await;
    assert_eq!(
        unknown.get("_tag"),
        Some(&serde_json::json!("UnknownError"))
    );
    assert_eq!(unknown.get("ref"), Some(&serde_json::json!("operation")));

    let conflict = ConflictErrorDto::new(
        Required::new(ConflictErrorDtoTag::ConflictError),
        Required::new(text("conflict")),
        OptionalNullable::value(text("session")),
    );
    let conflict = response_json(
        session_prompt_error_response(SessionPromptApplicationError::Conflict(conflict)),
        "conflict",
    )
    .await;
    assert_eq!(
        conflict.get("_tag"),
        Some(&serde_json::json!("ConflictError"))
    );
    assert_eq!(
        conflict.get("resource"),
        Some(&serde_json::json!("session"))
    );
}

#[test]
fn every_registered_endpoint_owns_its_exact_success_status() {
    let statuses: Vec<StatusCode> = Endpoint::ALL
        .iter()
        .map(|endpoint| endpoint.success_status())
        .collect();
    assert_eq!(statuses.len(), DOGFOOD_NON_SSE_ROUTE_COUNT);
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::NO_CONTENT)
            .count(),
        // Selection/context/reply writes, durable delete, and MCP registration,
        // configuration edits and reconnect settle without a response document.
        17
    );
    assert_eq!(
        Endpoint::SessionInterrupt.success_status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        Endpoint::SessionPermissionReply.success_status(),
        StatusCode::NO_CONTENT
    );
    assert!(Endpoint::ALL
        .iter()
        .filter(|endpoint| {
            !matches!(
                endpoint,
                Endpoint::SessionInterrupt
                    | Endpoint::McpAdd
                    | Endpoint::McpConfiguration
                    | Endpoint::McpReconnect
                    | Endpoint::SessionDelete
                    | Endpoint::SessionRename
                    | Endpoint::SessionAgentSelect
                    | Endpoint::SessionModelSelect
                    | Endpoint::SessionPresetSelect
                    | Endpoint::SessionSubagentModelSet
                    | Endpoint::SessionSubagentModelClear
                    | Endpoint::SessionPermissionReply
                    | Endpoint::SessionFormReply
                    | Endpoint::SessionFormCancel
                    | Endpoint::SessionCapabilityPut
                    | Endpoint::InstructionEntryPut
                    | Endpoint::InstructionEntryRemove
            )
        })
        .all(|endpoint| endpoint.success_status() == StatusCode::OK));
}

#[tokio::test]
async fn query_decoding_is_typed_bounded_and_duplicate_closed() {
    let location = request_for(
        Endpoint::LocationGet,
        request("/api/location?location%5Bdirectory%5D=%2Fwork%2Fproject"),
    )
    .await;
    match location {
        Ok(ApplicationRequest::LocationGet(query)) => {
            let value = serde_json::to_value(query)
                .unwrap_or_else(|error| panic!("location query did not encode: {error}"));
            assert_eq!(value["location"]["directory"], "/work/project");
        }
        other => panic!("location route lost the retained deep-object query: {other:?}"),
    }
    for uri in [
        "/api/location?surprise=x",
        "/api/location?location[directory]=a&location[directory]=b",
        "/api/location?location=null&location[directory]=a",
    ] {
        assert!(
            request_for(Endpoint::LocationGet, request(uri))
                .await
                .is_err(),
            "location route accepted malformed query {uri}"
        );
    }
    let decoded = request_for(
        Endpoint::SessionList,
        request("/api/session?order=desc&parentID=null&search=needle"),
    )
    .await;
    match decoded {
        Ok(ApplicationRequest::SessionList(query)) => {
            assert!(query.order().as_value().is_some());
            assert!(query.parent_id().as_value().is_some());
            assert!(query.search().as_value().is_some());
        }
        Ok(other) => panic!("wrong request variant: {other:?}"),
        Err(response) => panic!("query was refused with {}", response.status()),
    }

    let duplicate = request_for(
        Endpoint::SessionList,
        request("/api/session?order=asc&order=desc"),
    )
    .await;
    assert!(duplicate.is_err(), "duplicate query field was accepted");

    let unknown = request_for(
        Endpoint::ProjectCurrent,
        request("/api/project/current?surprise=x"),
    )
    .await;
    assert!(unknown.is_err(), "unknown query field was accepted");
}

#[tokio::test]
async fn percent_decoded_single_space_session_identity_is_refused() {
    let response = request_for(Endpoint::SessionGet, request("/api/session/%20")).await;
    let Err(response) = response else {
        panic!("percent-decoded single-space session identity reached the application");
    };
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "single-space session identity returned the wrong refusal"
    );
}

#[tokio::test]
async fn retained_question_list_keeps_location_validation() {
    assert_eq!(
        Endpoint::QuestionRequestList.path(),
        "/api/question/request"
    );
    let decoded = request_for(
        Endpoint::QuestionRequestList,
        request("/api/question/request?location[directory]=%2Fwork%2Fproject"),
    )
    .await;
    assert!(matches!(
        decoded,
        Ok(ApplicationRequest::QuestionRequestList(_))
    ));
    for uri in [
        "/api/question/request?surprise=x",
        "/api/question/request?location[directory]=a&location[directory]=b",
    ] {
        assert!(request_for(Endpoint::QuestionRequestList, request(uri))
            .await
            .is_err());
    }
    let response = serde_json::json!({"data":[],"location":{"directory":"/work/project","project":{"id":"prj_test","directory":"/work/project"}}});
    // No legacy producer exists: the wire type must never accept invented rows.
    assert!(
        serde_json::from_value::<super::super::QuestionListResponseDto>(response.clone()).is_ok()
    );
    let mut invalid = response;
    invalid["data"] = serde_json::json!([{}]);
    assert!(serde_json::from_value::<super::super::QuestionListResponseDto>(invalid).is_err());
}

#[tokio::test]
async fn a_route_cannot_run_without_the_authenticated_extension() {
    let state = Arc::new(RouteState {
        application: ApplicationBinding::Fixed(never_application()),
    });
    let response = handle(state, Endpoint::ProjectList, request("/api/project")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
}

#[path = "routes_form_tests.rs"]
mod form_tests;

#[path = "routes_tests/workspace_diff.rs"]
mod workspace_diff;
#[path = "routes_tests/workspace_worktree.rs"]
mod workspace_worktree;

#[path = "routes_tests/mcp_inspect.rs"]
mod mcp_inspect;

#[path = "routes_tests/encoding.rs"]
mod encoding;

#[path = "routes_tests/command_inspect.rs"]
mod command_inspect;

#[tokio::test]
async fn fork_cutoff_is_optional_typed_and_closed_before_dispatch() {
    for raw in [
        "",
        "{}",
        r#"{"throughSeq":42}"#,
        r#"{"beforeMessageID":"msg_cutoff"}"#,
    ] {
        let mut request = request("/api/session/ses_parent/fork");
        *request.body_mut() = Body::from(raw);
        let parsed = request_for(Endpoint::SessionFork, request)
            .await
            .unwrap_or_else(|_| panic!("valid fork body refused"));
        let ApplicationRequest::SessionFork { request, .. } = parsed else {
            panic!("wrong fork dispatch");
        };
        let value =
            serde_json::to_value(request).unwrap_or_else(|error| panic!("fork DTO: {error}"));
        assert_eq!(
            value.get("throughSeq").and_then(serde_json::Value::as_u64),
            if raw.contains("42") { Some(42) } else { None }
        );
    }
    for raw in [
        r#"{"throughSeq":null}"#,
        r#"{"throughSeq":-1}"#,
        r#"{"throughSeq":1.5}"#,
        r#"{"throughSeq":1,"extra":true}"#,
        r#"{"throughSeq":1,"throughSeq":2}"#,
    ] {
        let mut request = request("/api/session/ses_parent/fork");
        *request.body_mut() = Body::from(raw);
        assert!(request_for(Endpoint::SessionFork, request).await.is_err());
    }
}

#[tokio::test]
async fn renamed_launch_headers_preserve_old_clients_and_refuse_disagreement(
) -> Result<(), Box<dyn std::error::Error>> {
    let current = "x-grapt-launch";
    let legacy = "x-secode-launch"; // grapt-rename: keep
    for (headers, accepted) in [
        (vec![(legacy, "headless")], true),
        (vec![(current, "headless")], true),
        (vec![(current, "headless"), (legacy, "headless")], true),
        (vec![(current, "headless"), (legacy, "interactive")], false),
    ] {
        let mut builder = Request::builder().method("POST").uri("/api/session");
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        let built = request_for(Endpoint::SessionCreate, builder.body(Body::from("{}"))?).await;
        if accepted {
            let Ok(ApplicationRequest::SessionCreate { launch, .. }) = built else {
                panic!("valid launch headers refused");
            };
            assert_eq!(launch, Some(grapt_types::SessionLaunch::Headless));
        } else {
            let Err(response) = built else {
                panic!("conflicting launch headers admitted");
            };
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
    }
    Ok(())
}
