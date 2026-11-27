//! Published shell guidance must match the adapter's native command syntax.
use super::*;

#[test]
fn shell_catalog_names_the_native_shell_and_inherited_environment() -> Result<(), String> {
    let description = definitions()
        .into_iter()
        .find(|definition| definition.name == SHELL_TOOL)
        .and_then(|definition| definition.description)
        .ok_or("shell catalog description is absent")?;
    assert!(description.len() <= 512);
    assert!(description.contains("Check test OS support"));
    assert!(description.contains("shell_environment_policy"));
    assert!(description.contains("credentials removed"));
    if cfg!(windows) {
        let program = shell_program()?;
        assert!(program
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("cmd.exe")));
        assert!(description.contains("cmd.exe /D /S /C"));
        assert!(description.contains("double-quoted executable paths"));
        assert!(description.contains("&&"));
        assert!(!description.contains("/bin/sh"));
    } else {
        assert!(description.contains("/bin/sh -c"));
        assert!(description.contains("POSIX shell syntax"));
        assert!(!description.contains("cmd.exe"));
    }
    Ok(())
}

fn quoted_check(scratch: &Scratch, exit: u8) -> Result<String, String> {
    let directory = scratch.0.join("tools with spaces");
    std::fs::create_dir(&directory).map_err(|error| error.to_string())?;
    let script = directory.join(if cfg!(windows) {
        "check.cmd"
    } else {
        "check.sh"
    });
    let contents = if cfg!(windows) {
        format!("@echo GUIDANCE_CHECK\r\n@exit /b {exit}\r\n")
    } else {
        format!("#!/bin/sh\nprintf 'GUIDANCE_CHECK\\n'\nexit {exit}\n")
    };
    std::fs::write(&script, contents).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    let path = script.to_str().ok_or("test script path is not UTF-8")?;
    // cmd's executable syntax uses a DOS path; canonical Windows paths carry
    // the filesystem-only verbatim prefix.
    let path = if cfg!(windows) {
        path.strip_prefix(r"\\?\").unwrap_or(path)
    } else {
        path
    };
    Ok(format!("\"{path}\" && echo GUIDANCE_CONTINUED"))
}

async fn guided_run(scratch: &Scratch, command: &str) -> Result<Vec<u8>, String> {
    let shell_input = input(&serde_json::json!({"command":command,"timeout_ms":5000}).to_string())?;
    let prepared = prepare_effect(&scratch.0, SHELL_TOOL, &shell_input, &no_routes())?;
    let EffectTarget::Shell {
        command,
        program,
        environment,
    } = prepared.target
    else {
        return Err("shell preparation changed effect class".to_owned());
    };
    run_shell(
        &scratch.0,
        &command,
        &program,
        &environment,
        &shell_input,
        &[],
        None,
    )
    .await?
    .model_output()
}

#[tokio::test]
async fn shell_guidance_quoted_path_and_success_chain_run_on_the_native_adapter(
) -> Result<(), String> {
    let scratch = Scratch::new()?;
    let command = quoted_check(&scratch, 0)?;
    let output = guided_run(&scratch, &command).await?;
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("GUIDANCE_CHECK"), "{text}");
    assert!(text.contains("GUIDANCE_CONTINUED"), "{text}");
    assert!(text.ends_with("Command exited with code 0."), "{text}");
    Ok(())
}

#[tokio::test]
async fn shell_guidance_success_chain_stops_at_a_failed_native_command() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let command = quoted_check(&scratch, 7)?;
    let output = guided_run(&scratch, &command).await?;
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("GUIDANCE_CHECK"), "{text}");
    assert!(!text.contains("GUIDANCE_CONTINUED"), "{text}");
    assert!(text.ends_with("Command exited with code 7."), "{text}");
    Ok(())
}
