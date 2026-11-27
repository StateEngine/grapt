//! A real OS BrokenPipe reaches the CLI's output exit without a diagnostic.
use super::*;
use std::time::Duration;

#[tokio::test]
async fn preclosed_stdout_pipe_is_an_output_failure_without_profile_effects() -> TestResult {
    let root = Scratch::new("closed-stdout")?;
    for arguments in [
        vec!["--help"],
        vec!["service", "status", "--service", "service"],
    ] {
        // No reader exists even before spawn: EPIPE cannot race a reader exit.
        let (reader, writer) = std::io::pipe()?;
        drop(reader);
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"))
            .args(&arguments)
            .env_clear()
            .env("HOME", root.path())
            .env("USERPROFILE", root.path())
            .env("APPDATA", root.path())
            .env("LOCALAPPDATA", root.path())
            .env("XDG_CONFIG_HOME", root.path())
            .env("XDG_DATA_HOME", root.path())
            .env("PATH", root.path().join("no-programs"))
            .env("RUST_BACKTRACE", "1")
            .current_dir(root.path())
            .stdin(Stdio::null())
            .stdout(Stdio::from(writer))
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let output =
            tokio::time::timeout(Duration::from_secs(10), output.wait_with_output()).await??;
        assert_eq!(
            output.status.code(),
            Some(i32::from(ExitClass::Output.code())),
            "{arguments:?}"
        );
        assert!(output.stdout.is_empty());
        assert!(
            output.stderr.is_empty(),
            "{arguments:?}: {:?}",
            output.stderr
        );
        assert_eq!(std::fs::read_dir(root.path())?.count(), 0);
    }
    Ok(())
}
