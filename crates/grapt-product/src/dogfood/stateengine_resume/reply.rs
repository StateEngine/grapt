//! Keep StateEngine's JSON answer separate from its transport diagnostics.
use super::{ContinuityUnavailable as E, MAX_REPLY};
use grapt_mcp::ToolCallOutcome;
use serde_json::Value;

const COLD_RESUME: &str = "stateengine_resume({}) with no since";
/// StateEngine's refusal of the presented capability (absent, revoked,
/// superseded or unreadable) has three wire forms, each matched by its code
/// exactly, never searched for in the reason that follows:
/// - the legacy door's tool error, whose text leads with this code;
const CAPABILITY_INVALID: &str = "Error: capability-invalid:";
/// - the native host's typed tool error, an `se.error/v1` envelope;
const ERROR_SCHEMA: &str = "se.error/v1";
const ERROR_CODE: &str = "capability-invalid";
/// - the native host's JSON-RPC auth refusal, before any tool runs, with the
///   message leading with one of these codes. `grapt-mcp` renders it as
///   "the server refused: {message} (code {code})".
const SESSION_REQUIRED: i64 = -32001;
const AUTH_CODES: [&str; 2] = ["capability-invalid:", "session-required:"];
const STALE_DOOR_NOTICE: &str = concat!(
    "stale_door: StateEngine flagged this connection; ",
    "keep its identity binding and relaunch to load current code."
);

/// StateEngine's own recovery for a cursor it no longer holds.
pub(super) fn names_cold_resume(outcome: &ToolCallOutcome) -> bool {
    outcome.is_error() && outcome.text().contains(COLD_RESUME)
}

/// A tool error that refuses the capability, in the legacy or typed form.
fn refused_tool(outcome: &ToolCallOutcome) -> bool {
    let text = outcome.text();
    if text.starts_with(CAPABILITY_INVALID) {
        return true;
    }
    let typed = |body: &Value| {
        body.get("schema").and_then(Value::as_str) == Some(ERROR_SCHEMA)
            && body.get("code").and_then(Value::as_str) == Some(ERROR_CODE)
    };
    match outcome.structured() {
        Some(body) => typed(body),
        None => {
            text.len() <= MAX_REPLY
                && serde_json::from_str::<Value>(text).is_ok_and(|body| typed(&body))
        }
    }
}

/// A JSON-RPC refusal detail that refuses the capability before any tool ran.
pub(super) fn refused_call(detail: &str) -> bool {
    let suffix = format!(" (code {SESSION_REQUIRED})");
    detail
        .strip_prefix("the server refused: ")
        .and_then(|rest| rest.strip_suffix(suffix.as_str()))
        .is_some_and(|message| AUTH_CODES.iter().any(|code| message.starts_with(code)))
}

pub(super) fn decode(outcome: &ToolCallOutcome) -> Result<Value, E> {
    if outcome.is_error() {
        // A refused capability needs a rebind; anything else is the door.
        return Err(if refused_tool(outcome) {
            E::Capability
        } else {
            E::Transport
        });
    }
    if outcome.text().len() > MAX_REPLY {
        return Err(E::Bounds);
    }
    let structured = outcome.structured();
    let structured_bytes = structured
        .map(serde_json::to_vec)
        .transpose()
        .map_err(|_| E::Contract)?;
    if structured_bytes
        .as_ref()
        .is_some_and(|bytes| bytes.len() > MAX_REPLY)
    {
        return Err(E::Bounds);
    }
    let mut textual = None;
    let mut stale_door = false;
    let mut trailing_notice = false;
    for block in outcome.content() {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            return Err(E::Contract);
        }
        let text = block
            .get("text")
            .and_then(Value::as_str)
            .ok_or(E::Contract)?;
        if text.starts_with("stale_door: warning: ") {
            stale_door = true;
            continue;
        }
        if text.starts_with("StateEngine: ") {
            // A transport shout follows an answer; it cannot stand in for
            // one, and a JSON body after it would no longer be trailing.
            if textual.is_none() && structured.is_none() {
                return Err(E::Contract);
            }
            stale_door = true;
            trailing_notice = true;
            continue;
        }
        if textual.is_some() || trailing_notice {
            return Err(E::Contract);
        }
        let parsed: Value = serde_json::from_str(text).map_err(|_| E::Contract)?;
        if structured.is_some_and(|body| body != &parsed) {
            return Err(E::Contract);
        }
        textual = Some(parsed);
    }
    let mut body = structured.cloned().or(textual).ok_or(E::Contract)?;
    let object = body.as_object_mut().ok_or(E::Contract)?;
    if stale_door {
        // Preserve the notice, not raw diagnostics that may repeat private handles.
        // This local context field is never a second source of authority.
        if object.contains_key("grapt_transport_diagnostics") {
            return Err(E::Contract);
        }
        object.insert(
            "grapt_transport_diagnostics".to_owned(),
            Value::Array(vec![Value::String(STALE_DOOR_NOTICE.to_owned())]),
        );
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(super) fn decode_frame(frame: Value) -> Result<Value, E> {
        let bounds = grapt_mcp::McpBounds::conservative().map_err(|_| E::Bounds)?;
        let outcome = grapt_mcp::accept_call(bounds, &frame).map_err(|_| E::Contract)?;
        decode(&outcome)
    }

    #[test]
    fn a_refused_capability_needs_a_rebind_and_other_tool_errors_are_transport() {
        for reason in [
            "capability is revoked: this launch was stopped or superseded",
            "capability is unknown",
            "malformed request _meta",
        ] {
            let frame = json!({"isError": true, "content": [
                {"type": "text", "text": format!("Error: capability-invalid: {reason}")}
            ]});
            assert_eq!(decode_frame(frame), Err(E::Capability), "{reason}");
        }
        for text in [
            "Error: principal-metadata-conflict: anonymous and capability",
            "Error: tool failed; capability-invalid: quoted later is not the code",
            "capability-invalid: without the error prefix",
        ] {
            let frame = json!({"isError": true, "content": [{"type": "text", "text": text}]});
            assert_eq!(decode_frame(frame), Err(E::Transport), "{text}");
        }
    }

    #[test]
    fn json_and_named_diagnostics_keep_one_answer() -> Result<(), E> {
        let body = json!({"complete":true});
        for structured in [false, true] {
            let mut frame = json!({"content":[
                {"type":"text","text":body.to_string()},
                {"type":"text","text":"stale_door: warning: stateengine_resume; continue using the bound door"}
            ]});
            if structured {
                frame["structuredContent"] = body.clone();
            }
            let actual = decode_frame(frame)?;
            assert_eq!(actual["complete"], true);
            assert!(actual["grapt_transport_diagnostics"][0]
                .as_str()
                .is_some_and(|text| text.starts_with("stale_door:")));
        }
        assert_eq!(
            decode_frame(json!({"content":[],"structuredContent":body.clone()}))?,
            body
        );
        Ok(())
    }

    #[test]
    fn ambiguous_bodies_and_unrecognised_content_are_refused() {
        let body = json!({"complete":true});
        for frame in [
            json!({"content":[{"type":"text","text":body.to_string()},
                {"type":"text","text":body.to_string()}]}),
            json!({"content":[{"type":"text","text":body.to_string()}],
                "structuredContent":{"complete":false}}),
            json!({"content":[{"type":"text","text":"stale_door: warning: no answer"}]}),
            json!({"content":[{"type":"text","text":body.to_string()},
                {"type":"text","text":"unrecognised trailing text"}]}),
            json!({"content":[{"type":"image","data":"AA==","mimeType":"image/png"}]}),
            json!({"content":[{"type":"text","text":"null"}]}),
            json!({"content":[
                {"type":"text","text":json!({"grapt_transport_diagnostics":[]}).to_string()},
                {"type":"text","text":"stale_door: warning: collision"}
            ]}),
        ] {
            assert_eq!(decode_frame(frame), Err(E::Contract));
        }
    }

    #[test]
    fn errors_and_both_reply_representations_keep_their_bounds() {
        assert_eq!(
            decode_frame(json!({"isError":true,"content":[{"type":"text","text":"{}"}]})),
            Err(E::Transport)
        );
        for frame in [
            json!({"content":[{"type":"text","text":"x".repeat(MAX_REPLY+1)}]}),
            json!({"content":[],"structuredContent":{"oversized":"x".repeat(MAX_REPLY)}}),
        ] {
            assert_eq!(decode_frame(frame), Err(E::Bounds));
        }
    }
}

// Literal behind/unreadable outputs, with and without an answer refusal, from
// StateEngine src/process-staleness.mjs::doorStalenessShout. Fixture content
// is evidence of the wire contract; it is never a model-context instruction.
#[cfg(test)]
pub(super) fn real_shouts() -> Vec<Value> {
    serde_json::from_str(include_str!("fixtures/stale_door_shouts.json"))
        .unwrap_or_else(|error| panic!("real StateEngine shout fixture is malformed: {error}"))
}

#[cfg(test)]
#[path = "reply_shout_tests.rs"]
mod shout_tests;
