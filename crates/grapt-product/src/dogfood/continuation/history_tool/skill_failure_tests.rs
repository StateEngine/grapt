//! The budgeted model path retains only the same owned safe skill diagnostic.
use super::*;

#[test]
fn recent_skill_failure_retains_recovery_and_charges_it_without_foreign_text() -> Result<(), String>
{
    let root = std::env::temp_dir().join(format!("grapt-skill-failure-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let artifacts = FilesystemArtifactStore::open(
        &root,
        b"skill-failure".to_vec(),
        Box::new(grapt_store::SystemClock),
    )
    .map_err(|e| e.to_string())?;
    let detail = "Skill not found in the installed catalog. Nearest available skills: skill-creator. Use {} to list skills or query to search; use an exact installed id to load. A repository helper script is not automatically a skill; run its documented command instead.";
    let native = "shell failed (timeout)\nCause: shell timed out after 30000 ms\nStderr tail: last failure\nNext: Check effects before retrying.";
    for (tool, action, text, expected) in [
        ("skill", Some("skill"), detail.to_owned(), Some(detail)),
        ("skill", None, detail.to_owned(), Some(detail)),
        ("skill", Some("mcp_foreign"), detail.to_owned(), None),
        ("mcp_skill", Some("skill"), detail.to_owned(), None),
        ("skill", Some("skill"), format!("{detail} PRIVATE"), None),
        ("shell", Some("shell"), native.to_owned(), Some(native)),
        ("mcp_shell", Some("shell"), native.to_owned(), None),
        ("shell", None, native.to_owned(), Some(native)),
        ("shell", Some("mcp_foreign"), native.to_owned(), None),
        ("read", None, "read failed (preparation)\nCause: coding path is outside the workspace rooted at \\\\?\\C:\\work\nStderr tail: not applicable\nNext: Use shell/ssh for other trees.".to_owned(), Some(r"rooted at C:\work")),
        ("read", Some("read"), "read failed (read)\nCause: binary file\nStderr tail: not applicable\nNext: Choose a text file.".to_owned(), Some("binary file")),
    ] {
        let captured = artifacts
            .capture(
                std::io::Cursor::new(text.as_bytes()),
                grapt_artifact::CaptureBound::new(2048, 2048).ok_or("capture bound")?,
                grapt_artifact::ArtifactKind::CapturedOutput,
                grapt_artifact::Sensitivity::Ordinary,
            )
            .map_err(|e| e.to_string())?;
        let call = ToolCallId::new("call_skill_recovery").ok_or("call")?;
        let fact = session_tool_terminal::ToolTerminalFact::new(
            grapt_types::PhysicalAttemptId::new("pat_skill_recovery").ok_or("attempt")?,
            call.clone(),
            ToolTerminalOutcome::Failed {
                reason: grapt_types::ToolFailureReason::EffectFailed,
                detail: Some(
                    grapt_types::ToolTerminalArtifact::new(
                        captured.reference().clone(),
                        captured.disposition(),
                    )
                    .ok_or("terminal artifact")?,
                ),
            },
        );
        let calls = vec![StoredCall {
            seq: 1,
            message: "msg_skill_recovery".parse().map_err(|e| format!("{e}"))?,
            call,
            tool: Some(ToolName::new(tool).ok_or("tool")?),
            permission_tool: action.map(|name| ToolName::new(name).ok_or("action")).transpose()?,
            arguments: CodexToolText::try_new("{}".to_owned()).map_err(|e| e.to_string())?,
            execution: grapt_types::ToolExecution::Local,
            image_read: None,
        }];
        let mut remaining = recent::MAX_RECENT_MATERIALIZATION_BYTES;
        let outputs = assemble_outputs(
            vec![(2, StoredTerminal::Readable(fact.clone()))],
            &artifacts,
            Some((&mut remaining, &calls)),
            None,
        )
        .map_err(|e| e.to_string())?;
        let output = outputs.first().ok_or("output missing")?.output.as_str();
        if let Some(expected) = expected {
            assert!(output.contains(expected), "{output}");
        } else {
            assert_eq!(output, "Tool failed: EffectFailed");
        }
        assert!(!output.contains("PRIVATE"));
        assert!(!output.contains(r"\\?\"));
        assert_eq!(
            recent::MAX_RECENT_MATERIALIZATION_BYTES - remaining,
            recent::TOOL_ERROR_LABEL.len() + output.len() + 1
        );
        let mut exhausted = 0;
        assert!(assemble_outputs(
            vec![(2, StoredTerminal::Readable(fact.clone()))],
            &artifacts,
            Some((&mut exhausted, &calls)),
            None
        )
        .is_err());
    }
    drop(artifacts);
    std::fs::remove_dir_all(root).map_err(|e| e.to_string())?;
    Ok(())
}
