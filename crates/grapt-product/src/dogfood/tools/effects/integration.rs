//! Exact-argv integration commands share the session-owned shell supervisors.
use super::*;

pub(in crate::dogfood::tools) async fn run(
    root: PathBuf,
    argv: Vec<String>,
    control: ShellControl,
) -> Result<Vec<u8>, String> {
    const TIMEOUT_MS: u64 = 10 * 60 * 1_000;
    if control.cancellation.cancelled_by().is_some() {
        return Err("integration command cancelled before launch; no command ran".into());
    }
    let (program, arguments) = argv
        .split_first()
        .ok_or("integration command argv is empty")?;
    #[cfg(not(windows))]
    let outcome = {
        let mut command = tokio::process::Command::new(program);
        command
            .args(arguments)
            .current_dir(root)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let group = processkit::ProcessGroup::new()
            .map_err(|error| format!("integration command containment unavailable: {error}"))?;
        unix_shell::supervise_prepared(
            command,
            TIMEOUT_MS,
            Ok(group),
            &[],
            #[cfg(target_os = "linux")]
            None,
            Some(control),
        )
        .await?
    };
    #[cfg(windows)]
    let outcome = {
        let mut command = windows_spawn::Command::new(program);
        command.args(arguments).current_dir(root).env_clear();
        let system_root = std::env::var_os("SystemRoot")
            .ok_or("SystemRoot is unavailable for the integration command adapter")?;
        command.env("SystemRoot", system_root);
        windows_shell::supervise_prepared(command, None, TIMEOUT_MS, &[], Some(control)).await?
    };
    if outcome.status.success() && outcome.cleanup.is_ok() {
        if outcome.out.observed
            > u64::try_from(outcome.out.bytes.len())
                .map_err(|_| "integration command output length overflowed")?
        {
            return Err("integration command credential exceeded the output bound".into());
        }
        Ok(outcome.out.bytes)
    } else {
        let status = outcome.status;
        let cleanup = outcome.cleanup.clone();
        let output = outcome.model_output()?;
        let cleanup = cleanup.err().map_or(String::new(), |reason| {
            format!("; cleanup unconfirmed: {reason}")
        });
        Err(format!(
            "integration command exited with {status}{cleanup}; captured output: {}",
            String::from_utf8_lossy(&output)
        ))
    }
}
