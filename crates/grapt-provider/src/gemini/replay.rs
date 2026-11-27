//! Opaque signed parts stay bound to their original endpoint, account and model.
use crate::{replay::ReplayBinding, AdapterState};
use serde_json::{json, Value};

pub(crate) fn part(value: &Value) -> Result<Value, &'static str> {
    let object = value
        .as_object()
        .ok_or("Gemini replay part is not an object")?;
    if object.is_empty() || object.len() > 3 || value.to_string().len() > 128 * 1024 {
        return Err("Gemini replay part exceeds its bound");
    }
    if let Some(call) = object.get("functionCall") {
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "functionCall" | "thoughtSignature"))
            || call.as_object().is_none_or(|fields| fields.len() != 3)
            || call["id"]
                .as_str()
                .is_none_or(|id| id.is_empty() || id.len() > 256)
            || call["name"]
                .as_str()
                .is_none_or(|name| name.is_empty() || name.len() > 256)
            || !call["args"].is_object()
        {
            return Err("Gemini replay function call is malformed");
        }
    } else if !(object.len() == 1 && object.contains_key("thoughtSignature"))
        && (!value["text"].is_string()
            || object
                .keys()
                .any(|key| !matches!(key.as_str(), "text" | "thought" | "thoughtSignature"))
            || object
                .get("thought")
                .is_some_and(|value| !value.is_boolean()))
    {
        return Err("Gemini replay text part is malformed");
    }
    if object.get("thoughtSignature").is_some_and(|signature| {
        signature
            .as_str()
            .is_none_or(|text| text.is_empty() || text.len() > 64 * 1024)
    }) {
        return Err("Gemini thought signature is malformed");
    }
    Ok(value.clone())
}

pub(crate) fn state(value: &Value, binding: &ReplayBinding) -> Result<AdapterState, &'static str> {
    let value = part(value)?;
    AdapterState::retain(
        json!({"kind":"gemini-part-v1", "binding":binding.bytes(), "item":value}).to_string(),
    )
    .map_err(|_| "Gemini replay exceeds its byte bound")
}
