//! Unix shell supervision shares the effect owner and bounded capture path.
use super::*;

#[cfg(test)]
pub(in crate::dogfood::tools) async fn supervise(
    command: tokio::process::Command,
    timeout_ms: u64,
    group: Result<processkit::ProcessGroup, String>,
    credentials: &[String],
) -> Result<ShellOutcome, String> {
    supervise_prepared(
        command,
        timeout_ms,
        group,
        credentials,
        #[cfg(target_os = "linux")]
        None,
        None,
    )
    .await
}

/// The registered owner keeps this future alive until explicit cleanup completes.
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
pub(in crate::dogfood::tools) async fn supervise_prepared(
    mut command: tokio::process::Command,
    timeout_ms: u64,
    group: Result<processkit::ProcessGroup, String>,
    credentials: &[String],
    #[cfg(target_os = "linux")] pending: Option<sandbox::posix::Pending>,
    mut control: Option<ShellControl>,
) -> Result<ShellOutcome, String> {
    let mut notices = Vec::new();
    let receipt = control
        .as_ref()
        .map(|control| control.cleanup.clone())
        .unwrap_or_default();
    receipt.started();
    let (mut child, group) = match group {
        Ok(group) => {
            notices.extend(containment_notice(group.mechanism()).map(str::to_owned));
            let child = match group.spawn(command) {
                Ok(child) => child,
                Err(error) => {
                    let cleanup = receipt.failed_spawn(&group, &error).await;
                    return Err(match cleanup {
                        Ok(()) => format!("shell spawn failed: {error}"),
                        Err(cleanup) => format!(
                            "shell spawn failed: {error}; shell cleanup unconfirmed: {cleanup}"
                        ),
                    });
                }
            };
            (child, Some(group))
        }
        Err(error) => {
            notices.push(format!(
                "process containment is unavailable ({error}); processes this command starts may outlive it"
            ));
            let child = match command.spawn() {
                Ok(child) => child,
                Err(error) => {
                    let cleanup =
                        "shell containment is unavailable; failed launch cleanup is unconfirmed";
                    receipt.settled(Err(cleanup.into()));
                    return Err(format!("shell spawn failed: {error}; {cleanup}"));
                }
            };
            (child, None)
        }
    };
    // Observation timestamps are needed only by the Windows spawner.
    let _observed_ms = control.as_ref().and_then(|control| control.observed_ms);
    #[cfg(target_os = "linux")]
    let plan = pending.as_ref().map(|pending| pending.plan().clone());
    let captured = async {
        #[cfg(target_os = "linux")]
        let enforcement = if let Some(pending) = pending {
            let pid = child.id().ok_or("sandbox helper has no PID")?;
            tokio::select! {
                result = pending.finish(pid) => result?,
                () = shell_cancelled(&mut control) => {
                    return Err("shell cancelled during sandbox startup".into());
                }
            }
        } else {
            RealizedEnforcement::HarnessOnly
        };
        let stdout = child.stdout.take().ok_or("shell stdout was absent")?;
        let stderr = child.stderr.take().ok_or("shell stderr was absent")?;
        let waited = async {
            let (observed, cancelled) = tokio::select! {
                biased;
                exit = tokio::time::timeout(Duration::from_millis(timeout_ms), root_exited(&mut child)) => (exit.ok(), false),
                () = shell_cancelled(&mut control) => (None, true),
            };
            let (reaped, cleanup) = stop(
                &mut child,
                group.as_ref(),
                #[cfg(target_os = "linux")]
                plan.as_ref(),
            )
            .await;
            let exit = match observed {
                Some(Err(error)) => Some(Err(error)),
                Some(Ok(())) => Some(reaped),
                None if cancelled => Some(reaped),
                None => None,
            };
            ShellWait { exit, cleanup, cancelled, receipt: receipt.clone() }
        };
        let captured = collect_shell(stdout, stderr, waited, credentials, notices, timeout_ms).await;
        #[cfg(target_os = "linux")]
        let captured = captured
            .map(|mut outcome| {
                outcome.enforcement = enforcement.clone();
                outcome
            })
            .map_err(|reason| sandbox::failure(&enforcement, &reason, Some(true)));
        captured
    }
    .await;
    // Startup and pipe setup failures still owe an explicit stop and wait.
    if receipt.pending() {
        let cleanup = stop(
            &mut child,
            group.as_ref(),
            #[cfg(target_os = "linux")]
            plan.as_ref(),
        )
        .await
        .1;
        receipt.settled(cleanup.clone());
        if let Err(cleanup) = cleanup {
            let reason = captured
                .err()
                .unwrap_or_else(|| "shell setup failed".into());
            #[cfg(target_os = "linux")]
            return Err(sandbox::posix::failed_cleanup(&reason, &cleanup));
            #[cfg(not(target_os = "linux"))]
            return Err(format!("{reason}; shell cleanup unconfirmed: {cleanup}"));
        }
    }
    captured
}

async fn stop(
    child: &mut tokio::process::Child,
    group: Option<&processkit::ProcessGroup>,
    #[cfg(target_os = "linux")] plan: Option<&sandbox::Plan>,
) -> (
    std::io::Result<std::process::ExitStatus>,
    Result<(), String>,
) {
    // Always stop background jobs, even when the root exited successfully.
    let killed = group.map_or(Ok(()), |group| group.kill_all().map_err(|e| e.to_string()));
    let requested = if cfg!(target_os = "macos") && group.is_some() && killed.is_ok() {
        // The group kill includes the reserved leader. A redundant PID signal
        // can reject that leader once it is a zombie; census and reap still owe
        // proof that the whole original group stopped.
        Ok(())
    } else {
        child.start_kill().map_err(|error| error.to_string())
    };
    let deadline = tokio::time::Instant::now() + terminal::STOP_SETTLE_BOUND;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let confirmed = confirm(group, child.id(), deadline).await;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let confirmed = Err::<(), _>(
        "shell descendant exit confirmation is unavailable on this platform".to_owned(),
    );
    let reaped = match tokio::time::timeout_at(deadline, child.wait()).await {
        Ok(Ok(status)) => Ok(status),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(std::io::Error::other("shell root reap was not confirmed")),
    };
    #[cfg(target_os = "linux")]
    let settled = match plan {
        Some(plan) => sandbox::posix::settle(plan).await,
        None => Ok(()),
    };
    #[cfg(not(target_os = "linux"))]
    let settled = Ok(());
    let reaped_cleanup = reaped
        .as_ref()
        .map(|_| ())
        .map_err(|error| format!("shell root wait failed: {error}"));
    let cleanup = killed
        .and(requested)
        .and(confirmed)
        .and(reaped_cleanup)
        .and(settled);
    (reaped, cleanup)
}

async fn root_exited(child: &mut tokio::process::Child) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use nix::sys::wait::{waitid, Id, WaitPidFlag, WaitStatus};
        let pid = child
            .id()
            .and_then(|pid| i32::try_from(pid).ok())
            .ok_or_else(|| std::io::Error::other("shell root identity is unavailable"))?;
        loop {
            let status = waitid(
                Id::Pid(nix::unistd::Pid::from_raw(pid)),
                WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT,
            )
            .map_err(std::io::Error::from)?;
            match status {
                WaitStatus::Exited(_, _) | WaitStatus::Signaled(_, _, _) => return Ok(()),
                WaitStatus::StillAlive => tokio::time::sleep(Duration::from_millis(10)).await,
                _ => {
                    return Err(std::io::Error::other(
                        "shell exit observation was not terminal",
                    ))
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        terminal::macos_cleanup::root_exited(child).await
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    child.wait().await.map(|_| ())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn confirm(
    group: Option<&processkit::ProcessGroup>,
    leader: Option<u32>,
    deadline: tokio::time::Instant,
) -> Result<(), String> {
    let group =
        group.ok_or("shell containment is unavailable; descendant cleanup cannot be confirmed")?;
    loop {
        let running = match group.mechanism() {
            processkit::Mechanism::CgroupV2 => !group
                .members()
                .map_err(|error| format!("shell cgroup census failed: {error}"))?
                .is_empty(),
            #[cfg(target_os = "linux")]
            processkit::Mechanism::ProcessGroup => {
                terminal::linux_cleanup::group_running(
                    leader.ok_or("shell group leader identity is unavailable")?,
                    deadline,
                )
                .await?
            }
            #[cfg(target_os = "macos")]
            processkit::Mechanism::ProcessGroup => {
                terminal::macos_cleanup::group_running(
                    leader.ok_or("shell group leader identity is unavailable")?,
                    deadline,
                )
                .await?
            }
            _ => return Err("shell containment has no supported exit witness".into()),
        };
        if !running {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("shell descendants did not exit within the cleanup bound".into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
