//! Recover provider-neutral text syntax from the builtin's committed JSON input.
//! The registry owns the syntax; arbitrary plugin/MCP argument shapes cannot
//! impersonate a text call. Invalid wrappers keep their JSON; patch syntax
//! failures keep the builtin text format even before permission preparation.
use grapt_provider::{CodexInputItem, CodexToolText, ToolName};
use grapt_types::ToolCallId;

pub(in crate::dogfood) fn context_call(
    call: ToolCallId,
    tool: ToolName,
    arguments: CodexToolText,
    permission_tool: Option<&str>,
) -> Result<CodexInputItem, String> {
    if tool.as_str() == "apply_patch" && permission_tool.is_none_or(|action| action == "patch") {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments.as_str()) {
            if let Some(text) = value
                .as_object()
                .filter(|fields| fields.len() == 1)
                .and_then(|fields| fields.get("input"))
                .and_then(serde_json::Value::as_str)
            {
                return Ok(CodexInputItem::TextToolCall {
                    call,
                    tool,
                    input: CodexToolText::try_new(text.to_owned())
                        .map_err(|error| error.to_string())?,
                });
            }
        }
    }
    Ok(CodexInputItem::ToolCall {
        call,
        tool,
        arguments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_recovers_text_calls_without_reclassifying_other_tools() -> Result<(), String> {
        let call = ToolCallId::new("call_text").ok_or("call")?;
        let input = CodexToolText::try_new(serde_json::json!({"input":"patch\nλ"}).to_string())
            .map_err(|e| e.to_string())?;
        for (name, permission_tool, text) in [
            ("apply_patch", Some("patch"), true),
            ("apply_patch", Some("apply_patch"), false),
            ("apply_patch", None, true),
            ("mcp_echo", Some("mcp_echo"), false),
            ("mcp_echo", None, false),
            ("apply_patch", Some("read"), false),
        ] {
            let result = context_call(
                call.clone(),
                ToolName::new(name).ok_or("tool")?,
                input.clone(),
                permission_tool,
            )?;
            assert_eq!(matches!(result, CodexInputItem::TextToolCall { .. }), text);
        }
        Ok(())
    }
}
