//! Strict native wake state facts; authority transitions belong to admission.
use grapt_types::{EventId, SessionId};
use serde_json::Value;

pub(crate) struct NativeMailState {
    pub(crate) session_id: SessionId,
    pub(crate) held: bool,
    pub(crate) human_resumed: bool,
    pub(crate) target: Option<EventId>,
    pub(crate) binding: Option<String>,
}

pub(crate) fn decode_state(data: &Value) -> Result<NativeMailState, &'static str> {
    let object = data.as_object().ok_or("invalid native mail state")?;
    if object.len() != 5 {
        return Err("invalid native mail state fields");
    }
    let session_id = data
        .get("sessionID")
        .and_then(Value::as_str)
        .ok_or("invalid native mail session")?;
    let session_id: SessionId = session_id
        .parse()
        .map_err(|_| "invalid native mail session")?;
    let held = data
        .get("held")
        .and_then(Value::as_bool)
        .ok_or("invalid native mail pause")?;
    let human_resumed = data
        .get("humanResume")
        .and_then(Value::as_bool)
        .ok_or("invalid native mail resume")?;
    if held && human_resumed {
        return Err("contradictory native mail pause");
    }
    let target = match data.get("target") {
        Some(Value::Null) => None,
        Some(Value::String(value)) => {
            Some(value.parse().map_err(|_| "invalid native mail target")?)
        }
        _ => return Err("invalid native mail target"),
    };
    let binding = match data.get("binding") {
        Some(Value::Null) => None,
        Some(Value::String(value))
            if target.is_some()
                && value.len() == 64
                && value.bytes().all(|c| c.is_ascii_hexdigit()) =>
        {
            Some(value.clone())
        }
        _ => return Err("invalid native mail target binding"),
    };
    Ok(NativeMailState {
        session_id,
        held,
        human_resumed,
        target,
        binding,
    })
}

pub(crate) fn validate_state(data: &Value) -> Result<(), &'static str> {
    decode_state(data).map(|_| ())
}
