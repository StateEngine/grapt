//! Bounded request body decoding for authenticated Dogfood routes.

use axum::body::{to_bytes, Body};
use axum::http::Request;
use axum::response::Response;
use serde::de::DeserializeOwned;

use super::{invalid_request_response, unavailable_response, ApplicationUnavailable};
use crate::u3a::limits::MAX_REQUEST_DTO_BYTES;
use crate::u3a::{
    decode_image_prompt, decode_request, PromptRequestDto, SessionForkRequestDto, U3aLimits,
};

pub(super) async fn image_prompt_body(
    request: Request<Body>,
) -> Result<PromptRequestDto, Response> {
    let bytes = to_bytes(request.into_body(), 4 * 1024 * 1024)
        .await
        .map_err(|_| invalid_request_response("image prompt body exceeds its bound"))?;
    if bytes.is_empty() {
        return Err(invalid_request_response("request body is required"));
    }
    decode_image_prompt(&bytes).map_err(|_| invalid_request_response("invalid image prompt body"))
}

pub(super) async fn fork_body(request: Request<Body>) -> Result<SessionForkRequestDto, Response> {
    let bytes = to_bytes(request.into_body(), MAX_REQUEST_DTO_BYTES)
        .await
        .map_err(|_| invalid_request_response("request body exceeds its bound"))?;
    let limits = U3aLimits::production()
        .map_err(|_| unavailable_response(ApplicationUnavailable::try_new("dto-limits")))?;
    let bytes = if bytes.is_empty() {
        b"{}".as_slice()
    } else {
        &bytes
    };
    decode_request(bytes, limits).map_err(|_| invalid_request_response("invalid fork request body"))
}

pub(in crate::u3a) async fn body<T>(request: Request<Body>) -> Result<T, Response>
where
    T: DeserializeOwned,
{
    let bytes = to_bytes(request.into_body(), MAX_REQUEST_DTO_BYTES)
        .await
        .map_err(|_| invalid_request_response("request body exceeds its bound"))?;
    if bytes.is_empty() {
        return Err(invalid_request_response("request body is required"));
    }
    let limits = U3aLimits::production()
        .map_err(|_| unavailable_response(ApplicationUnavailable::try_new("dto-limits")))?;
    decode_request(&bytes, limits).map_err(|_| invalid_request_response("invalid request body"))
}
