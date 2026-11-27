use super::*;

#[tokio::test]
async fn preset_read_is_bodyless_and_rejects_bad_identity_or_query() {
    let parsed = request_for(
        Endpoint::SessionPresetGet,
        request("/api/session/ses_preset/preset"),
    )
    .await;
    match parsed {
        Ok(ApplicationRequest::SessionPresetGet(id)) => {
            assert_eq!(id.as_inner().as_str(), "ses_preset")
        }
        other => panic!("preset read lost its typed Session identity: {other:?}"),
    }
    assert_eq!(Endpoint::SessionPresetGet.success_status(), StatusCode::OK);
    for path in [
        "/api/session/%20/preset",
        "/api/session/not-session/preset",
        "/api/session/ses_preset/preset?surprise=1",
    ] {
        let Err(response) = request_for(Endpoint::SessionPresetGet, request(path)).await else {
            panic!("invalid preset read reached application dispatch: {path}");
        };
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[test]
fn preset_response_keeps_absent_selection_distinct_from_effective_trust() {
    use crate::u3a::{decode_request, SessionPresetResponseDto, U3aLimits};
    let limits = U3aLimits::production().unwrap_or_else(|error| panic!("DTO limits: {error}"));
    for selected in [serde_json::Value::Null, serde_json::json!("author")] {
        let document = serde_json::json!({"data":{"selected":selected,"effective":"inspect"}});
        let dto: SessionPresetResponseDto = decode_request(document.to_string().as_bytes(), limits)
            .unwrap_or_else(|error| panic!("valid preset response refused: {error}"));
        assert_eq!(
            serde_json::to_value(dto)
                .unwrap_or_else(|error| panic!("preset response did not encode: {error}")),
            document
        );
    }
    for document in [
        serde_json::json!({"data":{"effective":"maintainer"}}),
        serde_json::json!({"data":{"selected":null,"effective":"auto"}}),
        serde_json::json!({"data":{"selected":"auto","effective":"maintainer"}}),
        serde_json::json!({"data":{"selected":null,"effective":"maintainer","extra":1}}),
    ] {
        assert!(decode_request::<SessionPresetResponseDto>(
            document.to_string().as_bytes(),
            limits
        )
        .is_err());
    }
}

#[tokio::test]
async fn preset_read_requires_the_authenticated_extension() {
    let state = Arc::new(RouteState {
        application: ApplicationBinding::Fixed(never_application()),
    });
    let response = handle(
        state,
        Endpoint::SessionPresetGet,
        request("/api/session/ses_preset/preset"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
