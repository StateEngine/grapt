use super::super::{arguments, ExecSpec};
use super::*;
use crate::dogfood::tools::tests::Scratch;

fn fixture(directory: &Path, name: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let path = directory.join(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.into()
    });
    std::fs::write(&path, b"fixture").map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    std::fs::canonicalize(path).map_err(|error| error.to_string())
}

fn environment(
    root: &Path,
    paths: &[&Path],
    sandbox: bool,
) -> Result<shell_environment::Environment, String> {
    let path = std::env::join_paths(paths).map_err(|error| error.to_string())?;
    let config = serde_json::json!({"sandbox":{"enabled":sandbox,"network":false},"shell_environment_policy":{"inherit":"none","set":{
        "PATH": path.to_str().ok_or("fixture PATH is not UTF-8")?
    }}});
    std::fs::write(
        root.join("grapt.jsonc"),
        serde_json::to_vec(&config).map_err(|e| e.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    shell_environment::Environment::read(root, root, &[])
}

fn decode(
    root: &Path,
    shell: &str,
    env: shell_environment::Environment,
) -> Result<ExecSpec, String> {
    let value = serde_json::json!({"cmd":"echo fixture","shell":shell,"login":false});
    let input = BoundedToolInput::new(serde_json::to_vec(&value).map_err(|e| e.to_string())?)
        .map_err(|error| error.to_string())?;
    arguments::decode_exec(root, &input, env)
}

#[test]
fn public_shell_names_follow_prepared_path_order_and_policy() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let first = scratch.0.join("first");
    let second = scratch.0.join("second");
    for name in NAMES {
        let selected = fixture(&first, name)?;
        fixture(&second, name)?;
        let env = environment(&scratch.0, &[&first, &second], false)?;
        let spec = decode(&scratch.0, name, env)?;
        assert_eq!(spec.program, selected);
        assert_eq!(spec.cmd_shell, *name == "cmd");
        let env = environment(&scratch.0, &[&second], false)?;
        assert_eq!(
            decode(&scratch.0, name, env)?.program.parent(),
            Some(second.as_path())
        );
    }
    Ok(())
}

#[test]
fn discovery_and_errors_use_the_same_filtered_path_without_values() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let bin = scratch.0.join("bin");
    fixture(&bin, "pwsh")?;
    fixture(&bin, "sh")?;
    let env = environment(&scratch.0, &[&bin], false)?;
    assert_eq!(available(&env), "pwsh, sh");
    let description = super::super::exec_description(&env);
    assert!(description.contains("Available shells: pwsh, sh."));
    assert!(description.len() <= 512, "{description}");
    assert!(!description.contains(&bin.to_string_lossy().to_string()));
    let missing = bin.join(if cfg!(windows) { "bash.exe" } else { "bash" });
    let error = decode(
        &scratch.0,
        missing.to_str().ok_or("fixture path")?,
        env.clone(),
    )
    .err()
    .ok_or("missing shell accepted")?;
    assert!(
        error.contains("bash") && error.contains("available shells: pwsh, sh"),
        "{error}"
    );
    for name in ["./sh", "bin/sh", "pwsh.exe", "unrecognized"] {
        assert!(decode(&scratch.0, name, env.clone()).is_err(), "{name}");
    }
    let empty = environment(&scratch.0, &[], false)?;
    assert!(decode(&scratch.0, "sh", empty.clone()).is_err());
    assert!(available(&empty).starts_with("none on prepared PATH"));
    let absolute = bin.join(if cfg!(windows) { "sh.exe" } else { "sh" });
    assert!(decode(&scratch.0, absolute.to_str().ok_or("fixture path")?, empty).is_ok());
    for name in NAMES {
        fixture(&bin, name)?;
    }
    let all = [bin.as_path()];
    for sandbox in [false, true] {
        for paths in [&all[..], &[][..]] {
            let env = environment(&scratch.0, paths, sandbox)?;
            let description = super::super::exec_description(&env);
            assert!(
                description.len() <= 512,
                "description exceeds provider limit: {description}"
            );
            assert!(description.contains("login defaults false"));
            assert!(description.contains("msys_unconfined=true asks Once/Session consent"));
            assert_eq!(description.contains("request_sandbox_access"), sandbox);
        }
    }
    Ok(())
}

#[test]
fn relative_path_entries_and_directories_are_not_shells() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let bin = scratch.0.join("bin");
    std::fs::create_dir_all(bin.join(if cfg!(windows) { "sh.exe" } else { "sh" }))
        .map_err(|e| e.to_string())?;
    let env = environment(&scratch.0, &[Path::new("."), Path::new(""), &bin], false)?;
    assert!(decode(&scratch.0, "sh", env).is_err());
    #[cfg(unix)]
    {
        let path = fixture(&bin, "bash")?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        assert!(decode(&scratch.0, "bash", environment(&scratch.0, &[&bin], false)?).is_err());
    }
    Ok(())
}

#[test]
fn alias_failure_fixture_never_waives_missing_denied_or_ordinary_files() {
    let path = Path::new("/fixture/WindowsApps/Microsoft.PowerShell_8wekyb3d8bbwe/pwsh.exe");
    assert!(alias_failure(path, Some(1920), 0x400, false));
    for error in [2, 3, 5, 1314, 1921] {
        assert!(!alias_failure(path, Some(error), 0x400, false));
    }
    assert!(!alias_failure(path, Some(1920), 0x400, true));
    assert!(!alias_failure(path, Some(1920), 0, false));
    assert!(!alias_failure(path, Some(1920), 0x410, false));
    for path in [
        "/fixture/NotWindowsApps/pwsh.exe",
        "/fixture/WindowsApps/unknown.exe",
    ] {
        assert!(!alias_failure(Path::new(path), Some(1920), 0x400, false));
    }
}

#[cfg(windows)]
#[test]
#[ignore = "requires an explicit Windows App Execution Alias fixture; native execution needs READY/GO"]
fn native_windows_app_execution_alias_is_kept_for_create_process() -> Result<(), String> {
    let alias = std::env::var("GRAPT_TEST_SHELL_ALIAS").map_err(|e| e.to_string())?;
    let path = Path::new(&alias);
    let error = std::fs::canonicalize(path)
        .err()
        .ok_or("fixture is not an unresolved alias")?;
    assert!(
        execution_alias(path, &error),
        "fixture is not an accepted App Execution Alias"
    );
    let scratch = Scratch::new()?;
    let env = environment(&scratch.0, &[path.parent().ok_or("alias parent")?], false)?;
    assert_eq!(decode(&scratch.0, &alias, env.clone())?.program, path);
    assert_eq!(super::super::process::launch_path(path)?, path);
    let name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or("alias stem")?;
    assert_eq!(decode(&scratch.0, name, env)?.program, path);
    Ok(())
}
