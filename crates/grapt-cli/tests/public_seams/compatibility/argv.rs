//! OS argument decoding must refuse before profile or command effects.
use super::*;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;

#[tokio::test]
async fn non_unicode_arguments_are_sanitized_usage_errors_without_profile_effects() -> TestResult {
    let root = Scratch::new("non-unicode-argv")?;
    let invalid = OsString::from_vec(b"PRIVATE_ARG\xff\n\x1b[31m".to_vec());
    for prefix in [
        vec![],
        vec!["code", "--profile", "private-profile"],
        vec!["update", "--from"],
        vec!["run", "--service", "private-profile", "--prompt"],
    ] {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"));
        command
            .args(prefix)
            .arg(&invalid)
            .env_clear()
            .env("HOME", root.path())
            .env("USERPROFILE", root.path())
            .env("XDG_CONFIG_HOME", root.path())
            .env("XDG_DATA_HOME", root.path())
            .env("RUST_BACKTRACE", "1")
            .current_dir(root.path())
            .kill_on_drop(true);
        let output =
            tokio::time::timeout(std::time::Duration::from_secs(10), command.output()).await??;
        assert_eq!(
            output.status.code(),
            Some(i32::from(ExitClass::Usage.code()))
        );
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8(output.stderr)?,
            "grapt: command arguments must be valid Unicode\n"
        );
        assert_eq!(std::fs::read_dir(root.path())?.count(), 0);
    }
    Ok(())
}

#[tokio::test]
async fn valid_unicode_arguments_still_reach_the_command_parser() -> TestResult {
    let root = Scratch::new("unicode-argv")?;
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"))
        .arg("unknown-é-文件")
        .env_clear()
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .current_dir(root.path())
        .kill_on_drop(true)
        .output()
        .await?;
    assert_eq!(
        output.status.code(),
        Some(i32::from(ExitClass::Usage.code()))
    );
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8(output.stderr)?.contains("unknown command"));
    assert_eq!(std::fs::read_dir(root.path())?.count(), 0);
    Ok(())
}
