// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// . Modified for durable history paging.
//! Stable model-only shell previews; stored output and history stay exact.
use super::{CodexToolText, ToolCallId};

pub(super) const MAX_BYTES: usize = 32 * 1024;
// Inline identities are optional guidance, never a new identity validity bound.
const INLINE_ID_BYTES: usize = MAX_BYTES / 4;

pub(super) fn is_shell_tool(tool: &str) -> bool {
    matches!(
        tool,
        "shell" | "exec_command" | "write_stdin" | "request_sandbox_access"
    )
}

pub(super) fn is_previewed_tool(tool: &str) -> bool {
    is_shell_tool(tool) || !crate::dogfood::tools::BUILTIN_TOOL_NAMES.contains(&tool)
}

pub(super) fn project(
    tool: &str,
    call: &ToolCallId,
    output: &CodexToolText,
) -> Result<CodexToolText, String> {
    project_str(tool, call, output.as_str())
}

pub(super) fn project_str(
    tool: &str,
    call: &ToolCallId,
    raw: &str,
) -> Result<CodexToolText, String> {
    // Off-mode shell results retain the established plain model contract;
    // the immutable artifact still records HarnessOnly for history and UI.
    let receipt = (tool == "shell")
        .then(|| crate::dogfood::tools::sandbox::receipt(raw))
        .flatten();
    let plain = receipt.as_ref().and_then(|receipt| {
        (receipt["enforcement"]["kind"] == "harness_only" && receipt["ran"] == true)
            .then(|| receipt.get("output").and_then(serde_json::Value::as_str))
            .flatten()
    });
    let text = plain.unwrap_or(raw);
    let is_shell = is_shell_tool(tool);
    if !is_previewed_tool(tool) || text.len() <= MAX_BYTES {
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
    let label = if is_shell { "Shell" } else { "Tool" };
    let marker = format!(
        "\n[{label} output shortened for model context: {} bytes omitted from {} captured bytes. Full captured output remains available. Recover with {guidance}.]\n",
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

    #[test]
    fn external_mcp_tool_outputs_bound_inline_and_point_to_history() -> Result<(), String> {
        use grapt_artifact::{
            ArtifactKind, ArtifactStore, CaptureBound, FilesystemArtifactStore, ReadBudget,
            Sensitivity,
        };

        let bounds = grapt_mcp::McpBounds::conservative().map_err(|e| e.to_string())?;
        let workspace = std::env::temp_dir().join(format!(
            "grapt-mcp-preview-workspace-{}",
            std::process::id()
        ));
        let artifact_root =
            std::env::temp_dir().join(format!("grapt-mcp-preview-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&workspace);
        let _ = std::fs::remove_dir_all(&artifact_root);
        std::fs::create_dir_all(&workspace).map_err(|e| e.to_string())?;
        let store = FilesystemArtifactStore::open(
            &artifact_root,
            b"mcp-preview-test-key".to_vec(),
            Box::new(grapt_store::SystemClock),
        )
        .map_err(|e| e.to_string())?;
        let bound = CaptureBound::new(1024 * 1024, 64 * 1024).ok_or("capture bound")?;

        let success_payload = format!("MCP_HEAD\n{}\nMCP_TAIL", "m".repeat(48_000));
        let error_payload = format!("MCP_ERR_HEAD\n{}\nMCP_ERR_TAIL", "e".repeat(48_000));
        for (is_error, payload) in [(false, &success_payload), (true, &error_payload)] {
            let raw = serde_json::json!({
                "content": [{"type": "text", "text": payload}],
                "isError": is_error,
            });
            let outcome = grapt_mcp::accept_call_up_to(bounds, &raw, bounds.max_frame_bytes())
                .map_err(|e| e.to_string())?;
            assert_eq!(outcome.is_error(), is_error);
            let full = outcome.text().to_owned();
            assert!(full.len() > MAX_BYTES);

            // Tool terminal durable artifact stores the full payload for `history` recovery.
            let captured = store
                .capture(
                    std::io::Cursor::new(full.as_bytes()),
                    bound,
                    ArtifactKind::CapturedOutput,
                    Sensitivity::SecretBearing,
                )
                .map_err(|e| e.to_string())?;
            assert!(captured.disposition().is_complete());
            let page = store
                .fetch(
                    captured.reference(),
                    0,
                    ReadBudget::new(1024 * 1024).ok_or("read budget")?,
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(page.bytes(), full.as_bytes());

            // Model-visible inline summary stays <= 32 KiB even for long/escaped call IDs.
            let output = CodexToolText::try_new(full.clone()).map_err(|e| e.to_string())?;
            for id in ["mcp-call-1".to_owned(), "\"\\ESCAPE".repeat(2048)] {
                let call = ToolCallId::new(&id).ok_or("mcp call id")?;
                let preview = project("mcp_demo_search", &call, &output)?;
                assert!(preview.as_str().len() <= MAX_BYTES);
                assert!(preview
                    .as_str()
                    .contains("[Tool output shortened for model context:"));
                assert!(!preview.as_str().contains("sha256:"));
                if id == "mcp-call-1" {
                    assert!(preview
                        .as_str()
                        .contains("history({\"call\":\"mcp-call-1\",\"offset\":"));
                } else {
                    assert!(preview.as_str().contains("history({}) to list"));
                }
                assert_eq!(project("mcp_demo_search", &call, &preview)?, preview);
            }
            assert_eq!(output.as_str(), full);
        }

        let workspace_entries: Vec<_> = std::fs::read_dir(&workspace)
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        assert!(workspace_entries.is_empty());
        let _ = std::fs::remove_dir_all(&workspace);
        let _ = std::fs::remove_dir_all(&artifact_root);
        Ok(())
    }
}
