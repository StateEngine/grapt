//! The one-shot shell uses the same cleanup job as exec_command and ConPTY.
use super::*;
use crate::dogfood::tools::terminal::windows_diagnostics::{log_failure, Context};
use crate::dogfood::tools::terminal::windows_job::ConfirmedJob;
use std::os::windows::io::{AsHandle, OwnedHandle};
use windows_spawn::{Command, CreationFlags, Stdio};

#[cfg(test)]
pub(in crate::dogfood::tools) async fn supervise(
    command: Command,
    timeout_ms: u64,
    credentials: &[String],
    observed_ms: Option<u64>,
) -> Result<ShellOutcome, String> {
    let (_sender, cancellation) = ToolCancellation::channel();
    supervise_prepared(
        command,
        None,
        timeout_ms,
        credentials,
        Some(ShellControl {
            observed_ms,
            cancellation,
            cleanup: ShellCleanup::default(),
        }),
    )
    .await
}

pub(in crate::dogfood::tools) async fn supervise_prepared(
    mut command: Command,
    pending: Option<sandbox::windows::Pending>,
    timeout_ms: u64,
    credentials: &[String],
    mut control: Option<ShellControl>,
) -> Result<ShellOutcome, String> {
    let cwd = match command.get_current_dir() {
        Some(cwd) => cwd.to_path_buf(),
        None => std::env::current_dir()
            .map_err(|error| format!("shell workdir unavailable: {error}"))?,
    };
    let context = Context::new(
        std::path::Path::new(command.get_program()),
        None,
        &cwd,
        control.as_ref().and_then(|control| control.observed_ms),
        "one-shot",
    );
    let job = ConfirmedJob::create_observed(&context).map_err(log_failure)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let receipt = control
        .as_ref()
        .map(|control| control.cleanup.clone())
        .unwrap_or_default();
    let mut child = match job.spawn_tracked(&mut command, CreationFlags::NO_WINDOW, &context) {
        Ok(child) => child,
        Err(failure) => {
            // A failed resume can leave a suspended root inside the job.
            let terminated = job.terminate(1);
            let confirmed = confirm(&job).await;
            receipt.settled(terminated.and(confirmed));
            return Err(log_failure(failure));
        }
    };
    drop(context);
    receipt.started();
    let captured = async {
        let enforcement = if let Some(pending) = pending {
            tokio::select! {
                result = pending.finish(child.id()) => result?,
                () = shell_cancelled(&mut control) => {
                    return Err("shell cancelled during sandbox startup".into());
                }
            }
        } else {
            RealizedEnforcement::HarnessOnly
        };
        let captured = async {
            let stdout = child.stdout.take().ok_or("shell stdout was absent")?;
            let stderr = child.stderr.take().ok_or("shell stderr was absent")?;
            let stdout = tokio::process::ChildStdout::from_std(std::process::ChildStdout::from(
                duplicate(&stdout)?,
            ))
            .map_err(|error| format!("shell stdout registration failed: {error}"))?;
            let stderr = tokio::process::ChildStderr::from_std(std::process::ChildStderr::from(
                duplicate(&stderr)?,
            ))
            .map_err(|error| format!("shell stderr registration failed: {error}"))?;
            let waited = async {
                let (mut exit, cancelled) = tokio::select! {
                    biased;
                    exit = tokio::time::timeout(Duration::from_millis(timeout_ms), wait(&mut child)) => (exit.ok(), false),
                    () = shell_cancelled(&mut control) => (None, true),
                };
                let stopped = stop(&job, &mut child).await;
                if cancelled {
                    exit = Some(match &stopped {
                        Ok(status) => Ok(*status),
                        Err(reason) => Err(std::io::Error::other(reason.clone())),
                    });
                }
                let cleanup = stopped.map(|_| ());
                ShellWait { exit, cleanup, cancelled, receipt: receipt.clone() }
            };
            collect_shell(stdout, stderr, waited, credentials, Vec::new(), timeout_ms).await
        }
        .await;
        captured
            .map(|mut outcome| {
                outcome.enforcement = enforcement.clone();
                outcome
            })
            .map_err(|reason| sandbox::failure(&enforcement, &reason, Some(true)))
    }
    .await;
    if receipt.pending() {
        let cleanup = stop(&job, &mut child).await.map(|_| ());
        receipt.settled(cleanup.clone());
        if let Err(cleanup) = cleanup {
            let reason = captured
                .err()
                .unwrap_or_else(|| "shell setup failed".into());
            return Err(format!("{reason}; shell cleanup unconfirmed: {cleanup}"));
        }
    }
    captured
}

fn duplicate(source: &impl AsHandle) -> Result<OwnedHandle, String> {
    filedescriptor::FileDescriptor::dup(&source.as_handle())
        .and_then(|handle| handle.as_file())
        .map(OwnedHandle::from)
        .map_err(|error| format!("shell pipe duplication failed: {error}"))
}

async fn wait(child: &mut windows_spawn::Child) -> std::io::Result<std::process::ExitStatus> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn stop(
    job: &ConfirmedJob,
    child: &mut windows_spawn::Child,
) -> Result<std::process::ExitStatus, String> {
    let terminated = job.terminate(1);
    let reaped = match tokio::time::timeout(terminal::STOP_SETTLE_BOUND, wait(child)).await {
        Ok(Ok(status)) => Ok(status),
        Ok(Err(error)) => Err(format!("shell root wait failed: {error}")),
        Err(_) => Err("shell root exit was not confirmed".into()),
    };
    let confirmed = confirm(job).await;
    terminated
        .and(reaped)
        .and_then(|status| confirmed.map(|()| status))
}

async fn confirm(job: &ConfirmedJob) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + terminal::stop::CONFIRM_BOUND;
    loop {
        if job.exited()? {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(terminal::windows_job::unconfirmed(job.active()?));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
