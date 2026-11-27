//! Shell invocation.
//! Modified by Grapt: prepared PATH discovery and explicit absolute paths.
use super::super::*;

pub(super) fn resolve(
    requested: Option<&str>,
    command: &str,
    login: bool,
    environment: &shell_environment::Environment,
) -> Result<(PathBuf, Vec<String>, bool), String> {
    let (path, name) = match requested {
        Some(path) => {
            let path = PathBuf::from(path);
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("shell filename is not UTF-8")?
                .to_ascii_lowercase();
            (path, name)
        }
        None => (
            shell_program()?,
            if cfg!(windows) { "cmd" } else { "sh" }.into(),
        ),
    };
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    let (arguments, cmd_shell) = match name {
        "sh" | "bash" | "zsh" | "dash" => (
            vec![if login { "-lc" } else { "-c" }.into(), command.into()],
            false,
        ),
        "powershell" | "pwsh" => {
            let mut args = Vec::new();
            if !login {
                args.push("-NoProfile".into());
            }
            args.extend(["-Command".into(), command.into()]);
            (args, false)
        }
        "cmd" => (
            vec!["/D".into(), "/S".into(), "/C".into(), command.into()],
            true,
        ),
        _ => return Err(format!(
            "shell {:?} is unsupported; use sh, bash, zsh, dash, pwsh, powershell or cmd; available shells: {}",
            path, super::shell_path::available(environment)
        )),
    };
    let path =
        super::shell_path::resolve(path.to_str().ok_or("shell path is not UTF-8")?, environment)?;
    Ok((path, arguments, cmd_shell))
}
