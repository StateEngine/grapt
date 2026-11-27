//! Transport witnesses for the frozen per-form routes.
//!
//! Split out of `routes_tests.rs` so neither file crosses the workspace's
//! per-module ceiling. These read the same fixtures and helpers as their parent.

use super::*;
use crate::u3a::query;

#[tokio::test]
async fn global_form_routes_consume_retained_headers_with_query_precedence() {
    for (endpoint, suffix) in [
        (Endpoint::SessionFormGet, ""),
        (Endpoint::SessionFormState, "/state"),
        (Endpoint::SessionFormReply, "/reply"),
        (Endpoint::SessionFormCancel, "/cancel"),
    ] {
        for (query, expected) in [
            ("", "/project+one"),
            ("?location%5Bdirectory%5D=%2Fquery", "/query"),
        ] {
            let uri = format!("/api/session/global/form/frm_live{suffix}{query}");
            let request = Request::builder()
                .uri(uri)
                .header(CONTENT_TYPE, "application/json")
                .header("x-grapt-directory", "%2Fproject+one")
                .header("x-grapt-workspace", "wrk_live")
                .body(Body::from(r#"{"answer":{"confirm":"CONFIRMED"}}"#))
                .unwrap_or_else(|error| panic!("request: {error}"));
            let parsed = match request_for(endpoint, request).await {
                Ok(ApplicationRequest::GlobalFormGet { query, .. })
                | Ok(ApplicationRequest::GlobalFormState { query, .. })
                | Ok(ApplicationRequest::GlobalFormReply { query, .. })
                | Ok(ApplicationRequest::GlobalFormCancel { query, .. }) => query,
                _ => panic!("global form route refused its retained request"),
            };
            let location = parsed
                .location()
                .as_value()
                .unwrap_or_else(|| panic!("location absent"));
            assert_eq!(
                location.directory().as_value().map(|value| value.as_str()),
                Some(expected)
            );
            assert_eq!(
                location
                    .workspace()
                    .as_value()
                    .map(|value| value.as_inner().to_string()),
                Some("wrk_live".into())
            );
        }
    }
}

#[tokio::test]
async fn global_form_routes_ignore_retired_location_header_names() {
    let retired = ["open", "code"].concat();
    let request = Request::builder()
        .uri("/api/session/global/form/frm_live")
        .header(format!("x-{retired}-directory"), "%2Flegacy")
        .header(format!("x-{retired}-workspace"), "wrk_legacy")
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("request: {error}"));
    let Ok(ApplicationRequest::GlobalFormGet { query, .. }) =
        request_for(Endpoint::SessionFormGet, request).await
    else {
        panic!("global form request refused")
    };
    assert!(query.location().is_absent());
}

#[tokio::test]
async fn global_form_location_headers_refuse_ambiguous_and_oversized_values() {
    for values in [
        vec!["%GG".to_owned()],
        vec!["x".repeat(query::MAX_QUERY_BYTES + 1)],
        vec!["%2Fone".to_owned(), "%2Ftwo".to_owned()],
    ] {
        let mut builder = Request::builder().uri("/api/session/global/form/frm_live");
        for value in values {
            builder = builder.header("x-grapt-directory", value);
        }
        let request = builder
            .body(Body::empty())
            .unwrap_or_else(|error| panic!("request: {error}"));
        let Err(response) = request_for(Endpoint::SessionFormGet, request).await else {
            panic!("invalid header admitted")
        };
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn global_form_routes_preserve_location_without_constructing_a_session_id() {
    for (endpoint, path) in [
        (
            Endpoint::SessionFormGet,
            "/api/session/global/form/frm_global",
        ),
        (
            Endpoint::SessionFormState,
            "/api/session/global/form/frm_global/state",
        ),
        (
            Endpoint::SessionFormCancel,
            "/api/session/global/form/frm_global/cancel",
        ),
    ] {
        let uri = format!("{path}?location%5Bdirectory%5D=E%3A%2Fproject");
        let built = request_for(endpoint, request(&uri)).await;
        let (id, query) = match built {
            Ok(ApplicationRequest::GlobalFormGet { form_id, query })
            | Ok(ApplicationRequest::GlobalFormState { form_id, query })
            | Ok(ApplicationRequest::GlobalFormCancel { form_id, query }) => (form_id, query),
            _ => panic!("global form route did not preserve its global owner"),
        };
        assert_eq!(id.as_inner().to_string(), "frm_global");
        let location = query
            .location()
            .as_value()
            .unwrap_or_else(|| panic!("location was erased"));
        assert_eq!(
            location.directory().as_value().map(|value| value.as_str()),
            Some("E:/project")
        );
    }
}

/// Cancel is the one form write with NO payload in the frozen contract. If it
/// were routed through the shared body reader it would answer "request body is
/// required" to the only call a client ever makes, so a bodyless cancel has to
/// reach application-request construction rather than being refused in front of
/// it. The other four form routes are covered by the shared body witness above.
#[tokio::test]
async fn a_bodyless_cancel_is_admitted_because_the_frozen_route_has_no_payload() {
    let built = request_for(
        Endpoint::SessionFormCancel,
        request("/api/session/ses_live/form/frm_live/cancel"),
    )
    .await;
    let Ok(ApplicationRequest::SessionFormCancel {
        session_id,
        form_id,
    }) = built
    else {
        panic!("a bodyless cancel did not build its own application request");
    };
    assert_eq!(session_id.as_inner().to_string(), "ses_live");
    assert_eq!(form_id.as_inner().to_string(), "frm_live");
}

/// Every per-form route reads its id from the SAME path position, and the two
/// ids must not be confused with each other: a form read that recovered the
/// session id as its form id would read the wrong row and still look healthy.
#[tokio::test]
async fn every_per_form_route_recovers_both_ids_and_never_swaps_them() {
    for (endpoint, uri) in [
        (Endpoint::SessionFormGet, "/api/session/ses_a/form/frm_b"),
        (
            Endpoint::SessionFormState,
            "/api/session/ses_a/form/frm_b/state",
        ),
        (
            Endpoint::SessionFormCancel,
            "/api/session/ses_a/form/frm_b/cancel",
        ),
    ] {
        let built = request_for(endpoint, request(uri)).await;
        let (session_id, form_id) = match built {
            Ok(ApplicationRequest::SessionFormGet {
                session_id,
                form_id,
            })
            | Ok(ApplicationRequest::SessionFormState {
                session_id,
                form_id,
            })
            | Ok(ApplicationRequest::SessionFormCancel {
                session_id,
                form_id,
            }) => (session_id, form_id),
            _ => panic!("{endpoint:?} did not build a per-form application request"),
        };
        assert_eq!(
            session_id.as_inner().to_string(),
            "ses_a",
            "{endpoint:?} session id"
        );
        assert_eq!(
            form_id.as_inner().to_string(),
            "frm_b",
            "{endpoint:?} form id"
        );
    }
}

/// An unusable form id is refused by the transport, so a malformed id never
/// reaches the application as a lookup that would answer "no such form" — the
/// two are different answers and a client acts on them differently.
#[tokio::test]
async fn an_unparseable_form_id_is_refused_before_application_dispatch() {
    let response = request_for(
        Endpoint::SessionFormGet,
        request("/api/session/ses_a/form/%2F"),
    )
    .await;
    let Err(response) = response else {
        panic!("an unparseable form id reached application request construction");
    };
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

fn form_missing() -> FormNotFoundErrorDto {
    let Some(form_id) = WireId::parse(String::from("frm_missing")) else {
        panic!("form-not-found fixture identity was invalid");
    };
    FormNotFoundErrorDto::new(
        Required::new(FormNotFoundErrorDtoTag::FormNotFoundError),
        Required::new(form_id),
        Required::new(text("missing")),
    )
}

fn form_settled() -> FormAlreadySettledErrorDto {
    let Some(form_id) = WireId::parse(String::from("frm_settled")) else {
        panic!("form-already-settled fixture identity was invalid");
    };
    FormAlreadySettledErrorDto::new(
        Required::new(FormAlreadySettledErrorDtoTag::FormAlreadySettledError),
        Required::new(form_id),
        Required::new(text("settled")),
    )
}

fn form_invalid_answer() -> FormInvalidAnswerErrorDto {
    let Some(form_id) = WireId::parse(String::from("frm_answer")) else {
        panic!("form-invalid-answer fixture identity was invalid");
    };
    FormInvalidAnswerErrorDto::new(
        Required::new(FormInvalidAnswerErrorDtoTag::FormInvalidAnswerError),
        Required::new(form_id),
        Required::new(text("bad answer")),
    )
}

/// The three form refusals carry DIFFERENT statuses and each route admits only
/// its own set. Settled is 409 and never 404 — the form exists and the caller is
/// late, which is not the same as the form being gone. An invalid answer is 400
/// and never 409 — it can be corrected and retried, which a settled form cannot.
#[test]
fn each_form_route_emits_only_the_refusals_its_frozen_contract_admits() {
    assert_eq!(
        form_create_error_response(FormCreateApplicationError::SessionNotFound(not_found()))
            .status(),
        StatusCode::NOT_FOUND
    );
    let conflict = ConflictErrorDto::new(
        Required::new(ConflictErrorDtoTag::ConflictError),
        Required::new(text("conflict")),
        OptionalNullable::absent(),
    );
    assert_eq!(
        form_create_error_response(FormCreateApplicationError::Conflict(conflict)).status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        form_create_error_response(FormCreateApplicationError::InvalidRequest(invalid())).status(),
        StatusCode::BAD_REQUEST
    );

    assert_eq!(
        form_lookup_error_response(FormLookupApplicationError::FormNotFound(form_missing()))
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        form_lookup_error_response(FormLookupApplicationError::SessionNotFound(not_found()))
            .status(),
        StatusCode::NOT_FOUND
    );

    assert_eq!(
        form_reply_error_response(FormReplyApplicationError::FormAlreadySettled(form_settled()))
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        form_reply_error_response(FormReplyApplicationError::FormInvalidAnswer(
            form_invalid_answer()
        ))
        .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        form_reply_error_response(FormReplyApplicationError::FormNotFound(form_missing())).status(),
        StatusCode::NOT_FOUND
    );

    assert_eq!(
        form_cancel_error_response(FormCancelApplicationError::FormAlreadySettled(
            form_settled()
        ))
        .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        form_cancel_error_response(FormCancelApplicationError::FormNotFound(form_missing()))
            .status(),
        StatusCode::NOT_FOUND
    );
}

/// The settled refusal names the form it refused, so a client that drove two
/// forms can tell WHICH one it was late for rather than guessing.
#[tokio::test]
async fn the_settled_refusal_names_the_form_and_its_own_tag() {
    let body = response_json(
        form_reply_error_response(FormReplyApplicationError::FormAlreadySettled(form_settled())),
        "settled",
    )
    .await;
    assert_eq!(
        body.get("_tag"),
        Some(&serde_json::json!("FormAlreadySettledError"))
    );
    assert_eq!(body.get("id"), Some(&serde_json::json!("frm_settled")));
}

#[tokio::test]
async fn renamed_location_headers_preserve_old_clients_and_refuse_disagreement(
) -> Result<(), Box<dyn std::error::Error>> {
    for (current, legacy, value, other) in [
        (
            "x-grapt-directory",
            "x-secode-directory", // grapt-rename: keep
            "%2Ffixture",
            "%2Fother",
        ),
        (
            "x-grapt-workspace",
            "x-secode-workspace", // grapt-rename: keep
            "wrk_fixture",
            "wrk_other",
        ),
    ] {
        for (headers, accepted) in [
            (vec![(legacy, value)], true),
            (vec![(current, value)], true),
            (vec![(current, value), (legacy, value)], true),
            (vec![(current, value), (legacy, other)], false),
        ] {
            let mut builder = Request::builder().uri("/api/session/global/form/frm_live");
            for (name, value) in headers {
                builder = builder.header(name, value);
            }
            let built = request_for(Endpoint::SessionFormGet, builder.body(Body::empty())?).await;
            if accepted {
                let Ok(ApplicationRequest::GlobalFormGet { query, .. }) = built else {
                    panic!("valid location headers refused");
                };
                let location = query.location().as_value().ok_or("location absent")?;
                if current == "x-grapt-directory" {
                    assert_eq!(
                        location.directory().as_value().map(|v| v.as_str()),
                        Some("/fixture")
                    );
                } else {
                    assert_eq!(
                        location
                            .workspace()
                            .as_value()
                            .map(|v| v.as_inner().to_string()),
                        Some("wrk_fixture".into())
                    );
                }
            } else {
                let Err(response) = built else {
                    panic!("conflicting location headers admitted");
                };
                assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            }
        }
    }
    Ok(())
}
