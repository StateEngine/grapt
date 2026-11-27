//! SC-SEC-11: omitted login does not source startup profiles.
use super::*;

fn decode(root: &Path, login: Option<bool>) -> Result<ExecSpec, String> {
    let mut input = serde_json::json!({"cmd":"echo fixture"});
    if let Some(login) = login {
        input["login"] = login.into();
    }
    let input = BoundedToolInput::new(serde_json::to_vec(&input).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    arguments::decode_exec(root, &input, shell_environment::Environment::parent()?)
}

#[test]
fn omitted_login_is_nonlogin_and_explicit_choices_are_preserved() -> Result<(), String> {
    let scratch = super::super::super::tests::Scratch::new()?;
    for (choice, expected) in [(None, false), (Some(false), false), (Some(true), true)] {
        let spec = decode(&scratch.0, choice)?;
        assert_eq!(spec.login, expected, "login={choice:?}");
        #[cfg(unix)]
        assert_eq!(
            spec.arguments,
            [if expected { "-lc" } else { "-c" }, "echo fixture"]
        );
        #[cfg(windows)]
        {
            assert_eq!(spec.arguments, ["/D", "/S", "/C", "echo fixture"]);
            assert!(spec.cmd_shell);
        }
    }
    assert!(EXEC_DESCRIPTION.contains("login defaults false"));
    Ok(())
}

#[test]
fn shell_argv_preserves_posix_powershell_and_cmd_startup_contracts() -> Result<(), String> {
    let scratch = super::super::super::tests::Scratch::new()?;
    // Resolution validates a regular executable path, not its host format.
    // This tests Windows argv on Unix too, without claiming a Windows launch.
    for name in [
        "sh",
        "bash",
        "zsh",
        "dash",
        "pwsh.exe",
        "powershell.exe",
        "cmd.exe",
    ] {
        let path = scratch.0.join(name);
        std::fs::write(&path, b"fixture").map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        for login in [false, true] {
            let (_, argv, cmd_shell) = command::resolve(
                path.to_str(),
                "fixture",
                login,
                &shell_environment::Environment::parent()?,
            )?;
            let expected = match name {
                "cmd.exe" => vec!["/D", "/S", "/C", "fixture"],
                "pwsh.exe" | "powershell.exe" if !login => {
                    vec!["-NoProfile", "-Command", "fixture"]
                }
                "pwsh.exe" | "powershell.exe" => vec!["-Command", "fixture"],
                _ => vec![if login { "-lc" } else { "-c" }, "fixture"],
            };
            assert_eq!(argv, expected, "{name}, login={login}");
            assert_eq!(cmd_shell, name == "cmd.exe");
        }
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn omitted_login_does_not_source_isolated_marker_profile() -> Result<(), String> {
    let scratch = super::super::super::tests::Scratch::new()?;
    std::fs::write(
        scratch.0.join(".profile"),
        b"export GRAPT_LOGIN_MARKER=loaded\n",
    )
    .map_err(|e| e.to_string())?;
    for (choice, expected) in [
        (None, "absent"),
        (Some(false), "absent"),
        (Some(true), "loaded"),
    ] {
        let mut spec = decode(&scratch.0, choice)?;
        // Supply only this test's HOME to isolate the real shell's profile.
        // The isolated HOME makes the profile witness independent of the host.
        spec.arguments[1] = "printf '%s' \"${GRAPT_LOGIN_MARKER-absent}\"".into();
        let mut child = tokio::process::Command::new(&spec.program);
        child
            .args(&spec.arguments)
            .current_dir(&scratch.0)
            .env_clear()
            .env("HOME", &scratch.0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        // Reuse the production shell supervisor: bounded stream capture,
        // five-second execution deadline, group cleanup and drain grace.
        let group = processkit::ProcessGroup::new().map_err(|e| e.to_string())?;
        let output = supervise_shell(child, 5_000, Ok(group), &[])
            .await?
            .command_text()?;
        assert_eq!(
            String::from_utf8_lossy(&output),
            expected,
            "login={choice:?}"
        );
    }
    Ok(())
}
