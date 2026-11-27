//! Terminal options must not enlarge a command-only execution grant.
use super::*;

#[cfg(unix)]
#[test]
fn sec03_shell_alias_cannot_change_the_default_programs_argument_contract() -> Result<(), String> {
    let scratch = Scratch::new()?;
    for name in ["cmd", "powershell"] {
        let alias = scratch.0.join(name);
        std::os::unix::fs::symlink(shell_program()?, &alias).map_err(|error| error.to_string())?;
        let prepared = prepare_effect(
            &scratch.0,
            terminal::EXEC_COMMAND,
            &input(
                &serde_json::json!({"cmd":"echo prefix-controlled","shell":alias,"login":false})
                    .to_string(),
            )?,
            &no_routes(),
        )?;
        let EffectTarget::TerminalExec(spec) = prepared.target else {
            return Err("terminal preparation changed effect class".into());
        };
        assert_eq!(spec.program, shell_program()?);
        assert!(
            !terminal_prefix_grants(&spec),
            "aliased shell startup argv received a command-only prefix grant: {name}"
        );
    }
    Ok(())
}

#[test]
fn sec03_terminal_prefix_grants_require_the_default_nonlogin_shell() -> Result<(), String> {
    let scratch = Scratch::new()?;
    for login in [false, true] {
        let prepared = prepare_effect(
            &scratch.0,
            terminal::EXEC_COMMAND,
            &input(&serde_json::json!({"cmd":"echo prefix-controlled","login":login}).to_string())?,
            &no_routes(),
        )?;
        let EffectTarget::TerminalExec(spec) = prepared.target else {
            return Err("terminal preparation changed effect class".into());
        };
        // Native cmd always uses /D; login never enables an AutoRun profile.
        assert_eq!(
            terminal_prefix_grants(&spec),
            !login || cfg!(windows),
            "login startup received a command-only prefix grant"
        );
    }
    let shell = scratch.0.join(if cfg!(windows) { "cmd.exe" } else { "sh" });
    std::fs::write(&shell, b"fixture").map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    let prepared = prepare_effect(
        &scratch.0,
        terminal::EXEC_COMMAND,
        &input(
            &serde_json::json!({"cmd":"echo prefix-controlled","shell":shell,"login":false})
                .to_string(),
        )?,
        &no_routes(),
    )?;
    let EffectTarget::TerminalExec(spec) = prepared.target else {
        return Err("terminal preparation changed effect class".into());
    };
    assert!(
        !terminal_prefix_grants(&spec),
        "selected executable received a command-only prefix grant"
    );
    Ok(())
}
