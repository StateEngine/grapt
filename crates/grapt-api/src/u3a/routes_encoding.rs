//! One bounded success encoder and a small, nonrecursive typed refusal.
use super::super::{
    encode_page, DtoCodecError, DtoString, U3aLimits, UnknownErrorDto, UnknownErrorDtoTag,
    U3A_PRODUCTION_LIMITS,
};
use axum::http::{header::CONTENT_TYPE, StatusCode};
use axum::response::{IntoResponse, Response};
use grapt_schema::{OptionalNullable, Required};
use serde::Serialize;

pub(in crate::u3a) fn json<T: Serialize>(status: StatusCode, value: &T) -> Response {
    let limits = match U3aLimits::production() {
        Ok(limits) => limits,
        Err(_) => return refusal(DtoCodecError::Limits),
    };
    match encode_page(value, limits) {
        Ok(body) => (status, [(CONTENT_TYPE, "application/json")], body).into_response(),
        Err(error) => refusal(error),
    }
}

fn refusal(error: DtoCodecError) -> Response {
    let text = match error {
        DtoCodecError::PageBytes => format!(
            "Response exceeds the {}-byte encoded page bound",
            U3A_PRODUCTION_LIMITS.page_bytes
        ),
        DtoCodecError::Limits => "Response DTO limits are unavailable".into(),
        _ => "Response DTO encoding failed".into(),
    };
    let Ok(message) = DtoString::try_new(text) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(CONTENT_TYPE, "application/json")],
            r#"{"_tag":"UnknownError","message":"Response DTO encoding failed"}"#,
        )
            .into_response();
    };
    // The refusal is a few bounded literals, independent of the failed DTO.
    // It must not re-enter the success encoder that just refused the response.
    let error = UnknownErrorDto::new(
        Required::new(UnknownErrorDtoTag::UnknownError),
        Required::new(message),
        OptionalNullable::absent(),
    );
    let body = match serde_json::to_vec(&error) {
        Ok(body) => body,
        Err(_) => br#"{"_tag":"UnknownError","message":"Response DTO encoding failed"}"#.to_vec(),
    };
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [(CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

pub(super) fn bounded_literal(value: &'static str) -> Option<DtoString> {
    DtoString::try_new(value).ok()
}
