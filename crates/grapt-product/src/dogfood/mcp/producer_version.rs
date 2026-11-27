//! Recognize a producer refusal for retirement, never as proof of no effect.
use super::McpCallRefusal;
use grapt_mcp::{McpToolName, ToolCallOutcome};

pub(crate) fn refused(outcome: &Result<ToolCallOutcome, McpCallRefusal>) -> bool {
    let text = match outcome {
        Ok(result) if result.is_error() => result.text(),
        Err(McpCallRefusal::Protocol(detail)) => detail
            .strip_prefix("the server refused: ")
            .unwrap_or(detail),
        _ => return false,
    };
    let text = text.strip_prefix("Error: ").unwrap_or(text);
    text.starts_with("mixed-engine-versions:") || text.starts_with("stale-mcp-client:")
}

/// The named StateEngine reads are idempotent; acknowledgement and bulk
/// dismissal variants are writes even though they share a read-shaped tool.
pub(crate) fn replayable_read(tool: &McpToolName, arguments: &serde_json::Value) -> bool {
    let Some(arguments) = arguments.as_object() else {
        return false;
    };
    match tool.as_str() {
        "stateengine_msg_read" => true,
        "stateengine_msg_list" => arguments
            .get("dismiss")
            .is_none_or(|value| value == &serde_json::Value::Bool(false)),
        "stateengine_resume" => !arguments.contains_key("packet_id"),
        _ => false,
    }
}

/// Only msg_send currently documents first-send deduplication. A similarly
/// named field on an unrelated tool does not establish that contract.
pub(crate) fn accepts_send_key(tool: &McpToolName, catalog: &[grapt_mcp::DiscoveredTool]) -> bool {
    tool.as_str() == "stateengine_msg_send"
        && catalog
            .iter()
            .find(|item| item.name() == tool)
            .is_some_and(|item| {
                item.input_schema()["properties"]["idempotency_key"]["type"] == "string"
            })
}

/// Choose the key before any bytes leave, then retain the exact arguments for
/// a single replay. The attempt and call identify an effect, not its payload:
/// changing the payload under a supplied key must still conflict at the server.
pub(crate) fn prepare_send(
    arguments: &mut serde_json::Value,
    location: &str,
    call: &str,
    attempt: Option<&grapt_types::PhysicalAttemptId>,
) -> bool {
    let Some(arguments) = arguments.as_object_mut() else {
        return false;
    };
    if arguments.contains_key("msg_id") {
        return false;
    }
    if let Some(key) = arguments.get("idempotency_key") {
        return key.as_str().is_some_and(|key| !key.is_empty());
    }
    let Some(attempt) = attempt else {
        return false;
    };
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    for value in [
        "secode-stateengine-send-v1",
        location,
        attempt.as_str(),
        call,
    ] {
        digest.update(&(value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
    let key: String = digest
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    arguments.insert("idempotency_key".into(), serde_json::Value::String(key));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_mcp::{accept_call, McpBounds};
    use serde_json::json;

    #[test]
    fn only_explicit_error_prefix_requests_a_refresh() -> Result<(), Box<dyn std::error::Error>> {
        for (error, text, expected) in [
            (true, "mixed-engine-versions: older client", true),
            (true, "Error: mixed-engine-versions: older client", true),
            (true, "Error: stale-mcp-client: older client", true),
            (
                false,
                "mixed-engine-versions: successful tool output",
                false,
            ),
            (
                true,
                "tool ran; mixed-engine-versions: later failure",
                false,
            ),
            (true, "ConnectionReplaced", false),
        ] {
            let outcome = accept_call(
                McpBounds::conservative()?,
                &json!({
                    "content":[{"type":"text","text":text}], "isError":error
                }),
            )?;
            assert_eq!(refused(&Ok(outcome)), expected, "{text}");
        }
        assert!(refused(&Err(McpCallRefusal::Protocol(
            "the server refused: mixed-engine-versions: older client (code -32603)".into()
        ))));
        assert!(!refused(&Err(McpCallRefusal::Disconnected(
            "mixed-engine-versions:".into()
        ))));
        Ok(())
    }
}
