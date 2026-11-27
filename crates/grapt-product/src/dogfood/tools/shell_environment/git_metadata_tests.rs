use super::*;

#[test]
fn saved_msys_grant_cannot_remove_git_metadata_protection() -> Result<(), String> {
    let scratch = super::super::tests::Scratch::new()?;
    let workspace = scratch.0.join("workspace");
    let service = scratch.0.join("service");
    for directory in [&workspace, &service, &workspace.join(".git")] {
        std::fs::create_dir(directory).map_err(|error| error.to_string())?;
    }
    let session: SessionId = "ses_git_msys_grant"
        .parse()
        .map_err(|error| format!("{error}"))?;
    for protected in [false, true] {
        std::fs::write(
            workspace.join("grapt.jsonc"),
            serde_json::json!({"sandbox":{
                "enabled":true,"git_metadata_read_only":protected
            }})
            .to_string(),
        )
        .map_err(|error| error.to_string())?;
        for (tool, arguments) in [
            (SHELL_TOOL, serde_json::json!({"command":"echo contained"})),
            (
                terminal::EXEC_COMMAND,
                serde_json::json!({"cmd":"echo contained","tty":false}),
            ),
            (
                terminal::EXEC_COMMAND,
                serde_json::json!({"cmd":"echo contained","tty":true}),
            ),
        ] {
            let input = BoundedToolInput::new(
                serde_json::to_vec(&arguments).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            let prepared =
                prepare_session(&workspace, &service, tool, &input, &[], &session, true)?;
            let environment = match &prepared.target {
                EffectTarget::Shell { environment, .. } => environment,
                EffectTarget::TerminalExec(spec) => &spec.environment,
                _ => return Err("expected a process effect".into()),
            };
            if protected {
                assert!(
                    environment.sandbox().is_some(),
                    "saved grant removed isolation"
                );
                assert!(environment.msys_executables().is_empty());
                assert!(matches!(
                    environment.intended(),
                    RealizedEnforcement::PlatformSandbox { .. }
                ));
                assert_ne!(prepared.action, grapt_permission::SANDBOX_MSYS_ACTION);
            } else {
                assert!(environment.sandbox().is_none());
                assert!(matches!(
                    environment.intended(),
                    RealizedEnforcement::Degraded { .. }
                ));
                assert_eq!(prepared.action, grapt_permission::SANDBOX_MSYS_ACTION);
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
#[test]
fn explicit_msys_request_refuses_before_a_git_protected_command_runs() -> Result<(), String> {
    let scratch = super::super::tests::Scratch::new()?;
    std::fs::write(
        scratch.0.join("grapt.jsonc"),
        r#"{"sandbox":{"enabled":false,"git_metadata_read_only":true}}"#,
    )
    .map_err(|error| error.to_string())?;
    let session: SessionId = "ses_git_msys_request"
        .parse()
        .map_err(|error| format!("{error}"))?;
    for tool in [SHELL_TOOL, terminal::EXEC_COMMAND] {
        let key = if tool == SHELL_TOOL { "command" } else { "cmd" };
        let input = BoundedToolInput::new(
            serde_json::to_vec(&serde_json::json!({
                key:"echo escaped>escaped.txt", "msys_unconfined":true
            }))
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        for session_granted in [false, true] {
            let reason = prepare_session(
                &scratch.0,
                &scratch.0,
                tool,
                &input,
                &[],
                &session,
                session_granted,
            )
            .err()
            .ok_or("explicit MSYS request bypassed Git protection")?;
            assert!(
                reason.contains("cannot preserve read-only Git metadata"),
                "{reason}"
            );
        }
    }
    assert!(!scratch.0.join("escaped.txt").exists());
    Ok(())
}
