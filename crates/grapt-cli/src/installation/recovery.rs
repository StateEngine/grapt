//! The shipped Windows recovery operator reuses the reviewed script byte-for-byte.

use std::path::PathBuf;

pub(super) const HELP: &str = "usage: grapt recover-sign-in --profile <profile> [--restore-legacy]\n\nWindows only. Check that this account can recover the profile's sign-in state.\nQuit its applications, stop its service and back up the profile before using\n--restore-legacy to restore the older credential format. No provider call is made.";

struct Options {
    profile: PathBuf,
    restore: bool,
}

impl Options {
    fn parse(arguments: &[String]) -> Result<Self, String> {
        if arguments.len() > 4 {
            return Err(HELP.into());
        }
        let (mut profile, mut restore) = (None, false);
        let mut arguments = arguments.iter();
        while let Some(option) = arguments.next() {
            match option.as_str() {
                "--profile" if profile.is_none() => {
                    let value = arguments.next().filter(|value| {
                        !value.is_empty()
                            && !value.starts_with('-')
                            && value.len() <= 4096
                            && !value.chars().any(char::is_control)
                    });
                    profile = Some(PathBuf::from(value.ok_or(HELP)?));
                }
                "--restore-legacy" if !restore => restore = true,
                _ => return Err(HELP.into()),
            }
        }
        Ok(Self {
            profile: profile.ok_or(HELP)?,
            restore,
        })
    }
}

pub(super) async fn run(arguments: &[String]) -> Result<String, String> {
    if arguments == ["--help"] || arguments == ["-h"] {
        return Ok(HELP.into());
    }
    let options = Options::parse(arguments)?;
    #[cfg(windows)]
    return windows(options).await;
    #[cfg(not(windows))]
    {
        let _ = (options.profile, options.restore);
        Err("sign-in format recovery is available only on Windows".into())
    }
}

#[cfg(windows)]
async fn windows(options: Options) -> Result<String, String> {
    use std::{process::Stdio, time::Duration};

    if !options.profile.is_absolute() {
        return Err("recovery requires an absolute --profile path".into());
    }
    let system = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or("SystemRoot is unknown; recovery did not start")?;
    // An environment value is data, never PowerShell syntax. The compiled
    // script is the same operator used by the standalone recovery fixture.
    let script = format!(
        "try {{ & {{\n{}\n}} -Profile $env:GRAPT_RECOVERY_PROFILE{} }} catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit 2 }}",
        include_str!("../../../../scripts/windows/recover-legacy-sign-in.ps1"),
        if options.restore { " -RestoreLegacy" } else { "" },
    );
    let encoded = super::acl::encoded_command(&script);
    if encoded.len() > 30000 {
        return Err("compiled recovery operator exceeds its command bound".into());
    }
    let mut child = tokio::process::Command::from(grapt_service::launcher_environment::command(
        system.join("System32/WindowsPowerShell/v1.0/powershell.exe"),
    ))
    .args([
        "-NoProfile",
        "-NonInteractive",
        "-NoLogo",
        "-EncodedCommand",
        &encoded,
    ])
    .env("GRAPT_RECOVERY_PROFILE", options.profile)
    .env_remove("PSModulePath")
    .stdin(Stdio::null())
    .stdout(Stdio::inherit())
    .stderr(Stdio::inherit())
    .kill_on_drop(true)
    .spawn()
    .map_err(|error| format!("sign-in recovery cannot start: {:?}", error.kind()))?;
    match tokio::time::timeout(Duration::from_secs(30), child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(String::new()),
        Ok(Ok(_)) => Err("sign-in recovery refused; see the named error above".into()),
        Ok(Err(error)) => Err(format!("sign-in recovery wait failed: {:?}", error.kind())),
        Err(_) => {
            child
                .start_kill()
                .map_err(|error| format!("sign-in recovery cannot stop: {:?}", error.kind()))?;
            tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .map_err(|_| "sign-in recovery could not be reaped within five seconds")?
                .map_err(|error| format!("sign-in recovery reap failed: {:?}", error.kind()))?;
            Err("sign-in recovery exceeded thirty seconds".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_requires_an_explicit_switch_and_one_profile() -> Result<(), String> {
        let parse = |values: &[&str]| {
            Options::parse(&values.iter().map(|s| (*s).into()).collect::<Vec<_>>())
        };
        assert!(!parse(&["--profile", "private profile"])?.restore);
        assert!(parse(&["--profile", "private profile", "--restore-legacy"])?.restore);
        for invalid in [
            vec![],
            vec!["--restore-legacy"],
            vec!["--profile", ""],
            vec!["--profile", "bad\npath"],
            vec!["--profile", "--restore-legacy"],
            vec!["--profile", "one", "--profile", "two"],
            vec!["--profile", "one", "--restore-legacy", "--restore-legacy"],
        ] {
            assert!(parse(&invalid).is_err());
        }
        Ok(())
    }
}
