//! Fail-closed foreground tool subset for a volatile service composition.
//!
//! Only the session/harness data is volatile. Requested coding/shell/network
//! effects remain user effects; shell programs and the OS may retain their own
//! data. In particular, atomic coding replacements still stage beside the
//! requested file. No disk-backed checkpoint owner or artifact fallback is used.
use super::*;

pub(super) fn ensure_supported(tool: &str) -> Result<(), &'static str> {
    match tool {
        READ_TOOL
        | EDIT_TOOL
        | WRITE_TOOL
        | GLOB_TOOL
        | GREP_TOOL
        | READ_LINES_TOOL
        | LIST_TOOL
        | PATCH_TOOL
        | APPLY_PATCH_TOOL
        | SHELL_TOOL
        | HISTORY_TOOL
        | PLAN_TOOL
        | image::TOOL
        | web::FETCH_TOOL => Ok(()),
        _ => Err(
            "tool unavailable in ephemeral sessions: terminal/background processes, delegation, \
             MCP, plugins/auth, skills, CodeMode, and undo are disabled; no effect ran",
        ),
    }
}

pub(super) fn prepare(
    root: &Path,
    service: &Path,
    tool: &str,
    input: &BoundedToolInput,
    credentials: &[String],
    session: &SessionId,
    msys_session_granted: bool,
) -> Result<PreparedEffect, String> {
    ensure_supported(tool)?;
    if tool == SHELL_TOOL {
        // Configuration is read-only; bounded shell output remains in pipes.
        // Do not enter the terminal registry or its background process lifetime.
        return shell_environment::prepare_session(
            root,
            service,
            tool,
            input,
            credentials,
            session,
            msys_session_granted,
        );
    }
    // Neither plugins nor published external routes participate, including
    // before/after callbacks and plugin-originated integration commands.
    prepare_effect_inner(
        root,
        tool,
        input,
        &super::super::mcp::PublishedTools::default(),
        None,
        None,
        credentials,
    )
}

#[cfg(test)]
pub(super) fn run_coding(
    root: &Path,
    target: &coding::CodingTarget,
    input: &BoundedToolInput,
) -> Result<Vec<u8>, mcp_call::EffectFailure> {
    // Bypass Workspace::open entirely: it provisions a filesystem restore
    // journal even with a memory artifact backend. No before-images or patch
    // receipts are retained, so file undo cannot be offered for these effects.
    let output = coding::run(root, target, input).map_err(mcp_call::EffectFailure::from)?;
    if matches!(
        target,
        coding::CodingTarget::Write(_)
            | coding::CodingTarget::Overwrite { .. }
            | coding::CodingTarget::Patch(_)
    ) {
        coding::checkpoints::without_undo(output)
    } else {
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::Scratch;
    use super::*;

    fn input(raw: &str) -> Result<BoundedToolInput, String> {
        BoundedToolInput::new(raw.as_bytes().to_vec()).map_err(|error| error.to_string())
    }

    #[test]
    fn volatile_tool_preparation_refuses_external_and_background_calls() -> Result<(), String> {
        let scratch = Scratch::new()?;
        let service = scratch.0.join("unopened-service");
        let session = "ses_ephemeral_tools".parse().map_err(|_| "session")?;
        for (tool, raw) in [
            ("exec_command", r#"{"cmd":"echo secret"}"#),
            (
                "write_stdin",
                r#"{"session_id":"op_existing","chars":"secret"}"#,
            ),
            ("subagent", r#"{"task":"secret"}"#),
            ("spawn_agent", r#"{"message":"secret","task_name":"child"}"#),
            ("execute", r#"{"code":"secret"}"#),
            ("plugin_integration_command", r#"{"argv":["secret"]}"#),
            ("mcp_external", r#"{"secret":"value"}"#),
            ("undo", r#"{}"#),
        ] {
            let error = prepare(
                &scratch.0,
                &service,
                tool,
                &input(raw)?,
                &[],
                &session,
                false,
            )
            .err()
            .ok_or("unsafe tool prepared")?;
            assert!(error.contains("no effect ran"));
        }
        assert!(!service.exists());
        Ok(())
    }

    #[test]
    fn volatile_patch_preserves_user_files_without_checkpoint_capture() -> Result<(), String> {
        let scratch = Scratch::new()?;
        let service = scratch.0.join("unopened-service");
        let session = "ses_ephemeral_patch".parse().map_err(|_| "session")?;
        let input = input(
            r#"{"input":"*** Begin Patch\n*** Add File: user.txt\n+intentional effect\n*** End Patch\n"}"#,
        )?;
        let prepared = prepare(
            &scratch.0,
            &service,
            APPLY_PATCH_TOOL,
            &input,
            &[],
            &session,
            false,
        )?;
        let EffectTarget::Coding(target) = prepared.target else {
            return Err("patch did not prepare coding target".into());
        };
        let output = run_coding(&scratch.0, &target, &input).map_err(|error| error.detail)?;
        let output: serde_json::Value =
            serde_json::from_slice(&output).map_err(|e| e.to_string())?;
        assert!(output["undoUnavailable"]
            .as_str()
            .is_some_and(|s| s.contains("ephemeral")));
        assert_eq!(
            std::fs::read(scratch.0.join("user.txt")).map_err(|e| e.to_string())?,
            b"intentional effect\n"
        );
        assert!(!service.exists());
        Ok(())
    }
}
