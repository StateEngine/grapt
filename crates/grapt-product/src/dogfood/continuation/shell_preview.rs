// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// . Modified for durable history paging.
//! Stable model-only shell previews; stored output and history stay exact.
use super::{CodexToolText, ToolCallId};

const MAX_BYTES: usize = 32 * 1024;
// Inline identities are optional guidance, never a new identity validity bound.
const INLINE_ID_BYTES: usize = MAX_BYTES / 4;

pub(super) fn project(
    tool: &str,
    call: &ToolCallId,
    output: &CodexToolText,
) -> Result<CodexToolText, String> {
    // Off-mode shell results retain the established plain model contract;
    // the immutable artifact still records HarnessOnly for history and UI.
    let receipt = (tool == "shell")
        .then(|| crate::dogfood::tools::sandbox::receipt(output.as_str()))
        .flatten();
    let plain = receipt.as_ref().and_then(|receipt| {
        (receipt["enforcement"]["kind"] == "harness_only" && receipt["ran"] == true)
            .then(|| receipt.get("output").and_then(serde_json::Value::as_str))
            .flatten()
    });
    let text = plain.unwrap_or(output.as_str());
    if !matches!(
        tool,
        "shell" | "exec_command" | "write_stdin" | "request_sandbox_access"
    ) || text.len() <= MAX_BYTES
    {
        return CodexToolText::try_new(text.to_owned()).map_err(|error| error.to_string());
    }
    let identity = (call.as_str().len() <= INLINE_ID_BYTES)
        .then(|| serde_json::json!(call.as_str()).to_string())
        .filter(|encoded| encoded.len() <= INLINE_ID_BYTES);
    // Covers the fixed explanation, counts and byte offset after JSON escaping.
    let marker_reserve = identity.as_ref().map_or(0, String::len) + 512;
    let budget = MAX_BYTES - marker_reserve;
    let mut head = budget / 2;
    let mut tail = text.len() - (budget - head);
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    // An envelope's byte offsets differ from its decoded command text.
    let offset = if plain.is_some() { 0 } else { head };
    let guidance = identity.map_or_else(
        || format!("history({{}}) to list this session's calls, then history with this call's id and byte offset {offset}"),
        |identity| format!("history({{\"call\":{identity},\"offset\":{offset}}})"),
    );
    let marker = format!(
        "\n[Shell output shortened for model context: {} bytes omitted from {} captured bytes. Full captured output remains available. Recover with {guidance}.]\n",
        tail - head, text.len()
    );
    CodexToolText::try_new(format!("{}{marker}{}", &text[..head], &text[tail..]))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_preserves_utf8_ends_and_names_exact_history_recovery() -> Result<(), String> {
        let text = format!("HEAD{}TAIL\nExit code: 7", "界😀é".repeat(8000));
        let output = CodexToolText::try_new(text.clone()).map_err(|e| e.to_string())?;
        let call = ToolCallId::new("preview-call").ok_or("call")?;
        let projected = project("shell", &call, &output)?;
        assert!(projected.as_str().len() <= MAX_BYTES);
        assert!(projected.as_str().starts_with("HEAD"));
        assert!(projected.as_str().ends_with("TAIL\nExit code: 7"));
        assert!(projected
            .as_str()
            .contains("history({\"call\":\"preview-call\",\"offset\":"));
        assert_eq!(output.as_str(), text);
        assert_eq!(project("read_lines", &call, &output)?, output);
        assert_eq!(project("shell", &call, &projected)?, projected);
        Ok(())
    }

    #[test]
    fn off_mode_shell_receipts_preserve_plain_model_output() -> Result<(), String> {
        let text = "hello\n\nCommand exited with code 0.";
        let stored =
            serde_json::json!({"output":text,"enforcement":{"kind":"harness_only"},"ran":true})
                .to_string();
        let output = CodexToolText::try_new(stored.clone()).map_err(|e| e.to_string())?;
        let call = ToolCallId::new("off-shell").ok_or("call")?;
        assert_eq!(project("shell", &call, &output)?.as_str(), text);
        assert_eq!(output.as_str(), stored);
        Ok(())
    }

    #[test]
    fn long_and_escaped_valid_call_ids_never_refuse_a_bounded_preview() -> Result<(), String> {
        let output = CodexToolText::try_new(format!("HEAD{}TAIL", "x".repeat(40000)))
            .map_err(|e| e.to_string())?;
        for id in [
            "c".repeat(2048),
            "\"\\OFFSET".repeat(2048),
            "c".repeat(65536),
        ] {
            let call = ToolCallId::new(&id).ok_or("long call id")?;
            let preview = project("shell", &call, &output)?;
            assert!(preview.as_str().len() <= MAX_BYTES);
            assert!(preview.as_str().starts_with("HEAD"));
            assert!(preview.as_str().ends_with("TAIL"));
            if id.len() == 2048 {
                assert!(preview.as_str().contains(&id));
            } else {
                assert!(preview.as_str().contains("history({}) to list"));
            }
            assert_eq!(project("shell", &call, &preview)?, preview);
        }
        Ok(())
    }
}
