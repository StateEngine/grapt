//! Execute the approved shell adapter, not a mock command environment.

use super::super::effects::developer_help::{failures_with, Installed};
use super::*;
use std::ffi::OsString;
use std::path::Path;

#[cfg(not(windows))]
async fn run(
    scratch: &Scratch,
    text: &str,
    timeout_ms: u64,
) -> Result<effects::ShellOutcome, String> {
    let input = input(&serde_json::json!({"command": text, "timeout_ms": timeout_ms}).to_string())?;
    let prepared = prepare_effect(&scratch.0, SHELL_TOOL, &input, &no_routes())?;
    let EffectTarget::Shell {
        command,
        program,
        mut environment,
    } = prepared.target
    else {
        return Err("shell preparation changed effect class".to_owned());
    };
    environment.without_sandbox();
    run_shell(
        &scratch.0,
        &command,
        &program,
        &environment,
        &input,
        &[],
        None,
    )
    .await
}

#[cfg(not(windows))]
fn quoted(path: &std::path::Path) -> Result<String, String> {
    Ok(format!(
        "'{}'",
        path.to_str()
            .ok_or("compiler path is not UTF-8")?
            .replace('\'', "'\"'\"'")
    ))
}

#[cfg(not(windows))]
#[tokio::test]
async fn missing_rustc_has_an_installed_recovery_path() -> Result<(), String> {
    let scratch = Scratch::new()?;
    // Force the lookup failure even on hosts with a system compiler. This is
    // caller-supplied command text, not a change to the adapter's environment.
    let missing = "PATH='' rustc --version";
    let model = run(&scratch, missing, 10000)
        .await
        .map_err(|error| format!("missing-compiler probe: {error}"))?
        .model_output()?;
    let text = String::from_utf8_lossy(&model);
    assert!(text.contains("rustc: not found"), "{text}");
    assert!(text.contains("Discovered executable candidates"), "{text}");
    assert!(text.contains("Command exited with code 127."), "{text}");
    let installed = super::super::effects::developer_help::Installed::host();
    let compiler = installed.rustc.first().ok_or_else(|| {
        format!(
            "test host has no direct Rust compiler: {}",
            installed.guidance()
        )
    })?;
    assert!(
        text.contains(&compiler.to_string_lossy().into_owned()),
        "{text}"
    );
    let recovery = format!("{} --version", quoted(compiler)?);
    // This functional witness executes a real installed compiler, whose cold
    // libraries can be slow under parallel process tests. Use the same bounded
    // allowance as the real cache-tool journey below; shell deadlines have
    // their own timeout tests.
    let output = run(&scratch, &recovery, 60000)
        .await
        .map_err(|error| format!("installed compiler recovery: {error}"))?
        .model_output()?;
    let recovered = String::from_utf8_lossy(&output);
    assert!(recovered.contains("rustc "), "{recovered}");
    assert!(
        recovered.ends_with("Command exited with code 0."),
        "{recovered}"
    );
    assert!(shell_environment::Environment::parent()?
        .names()
        .iter()
        .any(|name| name == "HOME"));
    Ok(())
}

#[cfg(not(windows))]
#[tokio::test]
async fn developer_notices_never_change_machine_bytes_or_exit_refusal() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let command = "printf 'rustc: command not found\\n' >&2; exit 7";
    let machine = run(&scratch, command, 10000).await?.command_text();
    let Err(error) = machine else {
        return Err("failed command was reported as machine success".to_owned());
    };
    assert_eq!(
        error,
        "shell exited with exit status: 7; captured output: rustc: command not found\n"
    );
    let Err(tool_error) = run(&scratch, command, 10000).await?.tool_command_text() else {
        return Err("failed tool command was reported as success".to_owned());
    };
    assert!(tool_error.starts_with(&error), "{tool_error}");
    assert!(
        tool_error.contains("\n[stderr]\nrustc: command not found"),
        "{tool_error}"
    );
    assert!(!tool_error.contains("Discovered"), "{tool_error}");
    let model = run(&scratch, command, 10000).await?.model_output()?;
    let text = String::from_utf8_lossy(&model);
    assert!(text.starts_with("rustc: command not found\n"), "{text}");
    assert!(
        text.contains("[shell: The command could not find a developer tool on PATH."),
        "{text}"
    );
    assert!(text.ends_with("Command exited with code 7."), "{text}");
    Ok(())
}

#[cfg(not(windows))]
#[tokio::test]
async fn missing_home_explains_command_local_recovery() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let output = run(&scratch, "unset HOME; set -u; printf '%s' \"$HOME\"", 10000)
        .await?
        .model_output()?;
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("HOME is intentionally unset"), "{text}");
    assert!(
        text.contains("dedicated workspace-local directory"),
        "{text}"
    );
    assert!(!text.ends_with("Command exited with code 0."), "{text}");
    Ok(())
}

#[cfg(not(windows))]
#[tokio::test]
async fn original_cache_checks_run_with_explicit_tools_and_workspace_home() -> Result<(), String> {
    // The mock compiler still exercises real disk admission. A RAM gate's
    // tmpfs is bounded by memory, not the disk cache's 60 GB free-space floor.
    let scratch = match std::env::var_os("SECODE_RAM_DISK_CACHE") {
        Some(base) => Scratch::new_in(Path::new(&base))?,
        None => Scratch::new()?,
    };
    let installed = super::super::effects::developer_help::Installed::host();
    let compiler = installed
        .rustc
        .first()
        .ok_or("test host lacks a direct compiler")?;
    let cache = installed
        .sccache
        .first()
        .ok_or("test host lacks the shared compiler cache")?;
    let compiler_bin = compiler
        .parent()
        .ok_or("compiler path lacks its directory")?;
    let cache_bin = cache.parent().ok_or("cache path lacks its directory")?;
    let home = scratch.0.join("workspace-home");
    std::fs::create_dir(&home).map_err(|error| error.to_string())?;
    let tools = format!(
        "{}:{}:/usr/bin:/bin",
        compiler_bin.display(),
        cache_bin.display()
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("test source is not in the workspace")?;
    let temporary = home.join("tmp");
    std::fs::create_dir(&temporary).map_err(|error| error.to_string())?;
    let command = format!(
        "PATH={} HOME={} TMPDIR={} /usr/bin/python3 -m unittest discover -s {} -p test_cache.py",
        quoted(Path::new(&tools))?,
        quoted(&home)?,
        quoted(&temporary)?,
        quoted(&root.join("scripts/build"))?
    );
    let output = run(&scratch, &command, 60000).await?.model_output()?;
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("Ran "), "{text}");
    assert!(!text.contains("Ran 0 tests"), "{text}");
    assert!(text.contains("\nOK\n"), "{text}");
    assert!(text.ends_with("Command exited with code 0."), "{text}");
    assert!(!text.contains("HOME: unbound variable"), "{text}");
    assert!(!text.contains("rustc: command not found"), "{text}");
    Ok(())
}

fn binary(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}
fn executable(path: &Path) -> Result<(), String> {
    let parent = path.parent().ok_or("fixture path has no parent")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    // Discovery must not execute this file or inspect its contents.
    std::fs::write(path, b"NOT_AN_EXECUTABLE_CREDENTIAL_SENTINEL")
        .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn at(home: &Path, path: &Path) -> Installed {
    Installed::discover(|name| match name {
        "HOME" | "USERPROFILE" => Some(home.as_os_str().to_owned()),
        "PATH" => Some(path.as_os_str().to_owned()),
        "RUSTUP_HOME" => None,
        _ => panic!("discovery queried an unapproved environment name: {name}"),
    })
}

#[test]
fn discovery_finds_direct_tools_and_does_not_offer_rustup_shims() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let bin = scratch.0.join(".cargo/bin");
    for name in ["rustup", "rustc", "cargo", "sccache"] {
        executable(&bin.join(binary(name)))?;
    }
    let direct = scratch.0.join(".rustup/toolchains/project-version/bin");
    for name in ["rustc", "cargo"] {
        executable(&direct.join(binary(name)))?;
    }
    std::fs::write(
        scratch.0.join(".cargo/credentials.toml"),
        b"PERSONAL_CREDENTIAL_MUST_NOT_APPEAR",
    )
    .map_err(|error| error.to_string())?;
    let installed = at(&scratch.0, &bin);
    assert_eq!(installed.rustc, [direct.join(binary("rustc"))]);
    assert_eq!(installed.cargo, [direct.join(binary("cargo"))]);
    assert_eq!(installed.sccache, [bin.join(binary("sccache"))]);
    assert!(!installed.guidance().contains("CREDENTIAL"));
    assert!(!installed.guidance().contains("settings.toml"));
    Ok(())
}

#[test]
fn absent_and_unreadable_toolchain_locations_are_distinct() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let empty = at(&scratch.0, &scratch.0);
    assert!(empty.rustc.is_empty());
    assert!(!empty.guidance().contains("Discovery incomplete"));
    std::fs::create_dir(scratch.0.join(".rustup")).map_err(|error| error.to_string())?;
    std::fs::write(scratch.0.join(".rustup/toolchains"), b"not a directory")
        .map_err(|error| error.to_string())?;
    let unreadable = at(&scratch.0, &scratch.0);
    assert!(unreadable.rustc.is_empty());
    assert!(unreadable.guidance().contains("Discovery incomplete"));
    assert!(unreadable.guidance().contains("could not be listed"));
    Ok(())
}

#[test]
fn discovery_reports_its_first_over_bound_directory() -> Result<(), String> {
    let scratch = Scratch::new()?;
    for index in 0..=32 {
        std::fs::create_dir_all(scratch.0.join(format!(".rustup/toolchains/{index}/bin")))
            .map_err(|error| error.to_string())?;
    }
    let installed = at(&scratch.0, &scratch.0);
    assert!(installed
        .guidance()
        .contains("stopped at 32 directory entries"));
    Ok(())
}

#[test]
fn custom_rustup_home_is_checked_without_reading_settings() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let rustup = scratch.0.join("custom rustup");
    let compiler = rustup.join("toolchains/project/bin").join(binary("rustc"));
    executable(&compiler)?;
    let installed = Installed::discover(|name| match name {
        "RUSTUP_HOME" => Some(rustup.as_os_str().to_owned()),
        "PATH" => Some(OsString::new()),
        "HOME" | "USERPROFILE" => None,
        _ => panic!("discovery queried an unapproved environment name: {name}"),
    });
    assert_eq!(installed.rustc, [compiler]);
    Ok(())
}

#[test]
fn path_discovery_reports_truncation_and_ignores_relative_locations() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let paths = std::env::join_paths(
        std::iter::once(PathBuf::from("relative-bin"))
            .chain((0..32).map(|index| scratch.0.join(index.to_string()))),
    )
    .map_err(|error| error.to_string())?;
    let installed = Installed::discover(|name| (name == "PATH").then(|| paths.clone()));
    assert!(installed.rustc.is_empty());
    assert!(installed
        .guidance()
        .contains("PATH discovery stopped at 32 directories"));
    Ok(())
}

#[test]
fn windows_missing_command_gets_the_same_recovery_guidance() {
    let notices = failures_with(
        b"'rustc' is not recognized as an internal or external command",
        b"",
        Installed::default,
    );
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("absolute path"));
}

#[test]
fn original_failure_gets_tool_paths_and_explicit_home_guidance() {
    let notices = failures_with(
        b"",
        b"build-cache.sh: line 5: rustc: command not found\nHOME: unbound variable",
        Installed::default,
    );
    assert_eq!(notices.len(), 2);
    assert!(notices[0].contains("rustc: not found in the checked locations"));
    assert!(notices[0].contains("set PATH explicitly"));
    assert!(notices[1].contains("dedicated workspace-local directory"));
    assert!(notices[1].contains("HOME is intentionally unset"));
}

#[test]
fn ordinary_failures_do_not_trigger_discovery() {
    let notices = failures_with(b"", b"assertion failed", || {
        panic!("an unrelated failure caused tool discovery")
    });
    assert!(notices.is_empty());
    let home = failures_with(b"", b"HOME: unbound variable", || {
        panic!("a missing HOME alone caused tool discovery")
    });
    assert_eq!(home.len(), 1);
}
