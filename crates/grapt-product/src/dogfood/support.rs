use grapt_api::u3a::SessionActiveApplicationError;
use grapt_api::u3a::{
    ApplicationFuture, ConflictErrorDto, InvalidRequestErrorDto, SessionNotFoundErrorDto,
    UnknownErrorDto,
};
use grapt_inbox::{AdmitOutcome, WakeDisposition};
use grapt_types::SessionId;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;

use super::{CatalogProjection, PUBLIC_CONTENT_BYTES};

/// Resolve one frozen location-query shape and attach catalog data. Production
/// plugin-aware catalog paths compose their transforms after this projection;
/// location-port witnesses retain this small untransformed helper.
#[allow(
    dead_code,
    reason = "retained by the location-port witness implementation"
)]
pub(super) fn catalog_response<T, Q, E>(
    catalog: Arc<CatalogProjection>,
    query: Q,
    data: Value,
    refusal: fn(String) -> E,
) -> ApplicationFuture<T, E>
where
    T: DeserializeOwned + Send + 'static,
    Q: Serialize + Send + 'static,
    E: Send + 'static,
{
    Box::pin(async move {
        let query = serde_json::to_value(query).map_err(|error| refusal(error.to_string()))?;
        let query = decode(query).map_err(refusal)?;
        let location = super::locations::resolve(&catalog, &query)
            .await
            .map_err(refusal)?;
        decode(json!({"location": location.wire(), "data": data})).map_err(refusal)
    })
}

pub(super) fn model(catalog: &CatalogProjection) -> Value {
    json!({
        "id": catalog.model,
        "modelID": catalog.model,
        "providerID": catalog.provider,
        "name": "Live Headless Model",
        "package": "builtin:live",
        "capabilities": {"tools": false, "input": ["text"], "output": ["reasoning", "text"]},
        "variants": [],
        "settings": {"metadata":{"default_effort": catalog.default_effort.as_ref().map(|effort| effort.as_str())}},
        "time": {"released": 0.0},
        "cost": [],
        "status": "active",
        "enabled": true,
        "limit": {"context": 65536, "output": 65536},
    })
}

pub(super) fn ready<T, E>(result: Result<T, E>) -> ApplicationFuture<T, E>
where
    T: Send + 'static,
    E: Send + 'static,
{
    Box::pin(async move { result })
}

pub(super) fn compose_prompt_extensions(
    request: &Value,
    mut text: String,
) -> Result<String, String> {
    if request
        .get("agents")
        .is_some_and(|value| !value.as_array().is_some_and(Vec::is_empty))
    {
        return Err("prompt agents content is not composed".to_owned());
    }
    if let Some(metadata) = request.get("metadata") {
        let fields = metadata
            .as_object()
            .ok_or("prompt metadata must be an object")?;
        if fields.is_empty()
            || fields
                .keys()
                .any(|key| !matches!(key.as_str(), "origin" | "maxSteps"))
            || fields
                .get("origin")
                .is_some_and(|origin| origin.as_str() != Some("seat-startup"))
        {
            return Err("prompt metadata is not composed".to_owned());
        }
    }
    if request.get("files").is_some_and(|value| !value.is_array()) {
        return Err("prompt files must be an array".to_owned());
    }
    for file in request
        .get("files")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let uri = file
            .get("uri")
            .and_then(Value::as_str)
            .ok_or_else(|| "prompt file URI is absent".to_owned())?;
        if uri.starts_with("data:image/") {
            let name = file.get("name").and_then(Value::as_str).unwrap_or("image");
            if name.len() > 256 || name.chars().any(char::is_control) {
                return Err(
                    "image attachment name exceeds its bound or contains control characters".into(),
                );
            }
            text.push_str("\n\n[Image attached: ");
            text.push_str(name);
            text.push(']');
            continue;
        }
        let name = file
            .get("name")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("attachment");
        text.push_str("\n\n[ACP attachment: ");
        text.push_str(name);
        text.push_str("]\n");
        text.push_str(uri);
    }
    grapt_api::u3a::PromptText::try_new(text.clone())
        .map(|_| text)
        .map_err(|_| "prompt plus file references exceeds its text bound".to_owned())
}

pub(super) fn decode<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

pub(super) fn invalid(message: impl ToString) -> InvalidRequestErrorDto {
    static_dto(json!({
        "_tag": "InvalidRequestError",
        "message": bounded_message(message),
    }))
}

/// Why an active-session read failed: a busy reader or a genuine bad request.
///
/// `read_projection` names a capacity wait with `ReadRefusal::Busy`'s own
/// sentence, taken from the same place this classifier reads it, so the two cannot
/// drift apart. A busy reader becomes `Busy` and the route answers 503 with
/// `Retry-After`; anything else stays an invalid request.
pub(super) fn active_read_failure(detail: &str) -> SessionActiveApplicationError {
    if detail == super::read_projection::ReadRefusal::Busy.detail() {
        SessionActiveApplicationError::Busy
    } else {
        SessionActiveApplicationError::InvalidRequest(invalid(detail))
    }
}

pub(super) fn not_found(session: &SessionId) -> SessionNotFoundErrorDto {
    static_dto(json!({
        "_tag": "SessionNotFoundError",
        "sessionID": session.as_str(),
        "message": "Session was not found",
    }))
}

pub(super) fn conflict(message: impl ToString) -> ConflictErrorDto {
    static_dto(json!({
        "_tag": "ConflictError",
        "message": bounded_message(message),
        "resource": "execution",
    }))
}

pub(super) fn unknown(message: impl ToString) -> UnknownErrorDto {
    static_dto(json!({
        "_tag": "UnknownError",
        "message": bounded_message(message),
    }))
}

pub(super) fn bounded_message(message: impl ToString) -> String {
    let message = message.to_string();
    let mut end = message.len().min(1_024);
    while end > 0 && !message.is_char_boundary(end) {
        end -= 1;
    }
    message[..end].to_owned()
}

pub(super) fn should_execute(outcome: &AdmitOutcome) -> bool {
    matches!(
        outcome,
        AdmitOutcome::Admitted {
            wake: WakeDisposition::WakeAdvised,
            ..
        } | AdmitOutcome::Reconciled {
            wake: WakeDisposition::WakeAdvised,
        }
    )
}

pub(super) fn public_content(value: &str) -> &str {
    let mut end = value.len().min(PUBLIC_CONTENT_BYTES);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

pub(super) fn static_dto<T: DeserializeOwned>(value: Value) -> T {
    match serde_json::from_value(value) {
        Ok(value) => value,
        Err(error) => unreachable!("fixed Dogfood DTO failed to decode: {error}"),
    }
}
