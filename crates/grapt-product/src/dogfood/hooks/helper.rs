//! Contained configured helpers whose owner survives a dropped caller.
use std::future::Future;
use std::process::ExitStatus;
use std::time::Duration;

use crate::dogfood::tools::ShellControl;
use grapt_runtime::ToolCancellation;
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::time::Instant;

const CLEANUP_BOUND: Duration = Duration::from_secs(4);
const REAP_BOUND: Duration = Duration::from_secs(2);

pub(in crate::dogfood) struct Control<'a> {
    pub owner: Option<&'a ShellControl>,
    pub cancellation: Option<&'a ToolCancellation>,
    pub deadline: Instant,
}

/// Exchange futures own their pipes and never detach work. On cancellation they
/// close those pipes before cleanup observes the tree and reaps the leader.
pub(in crate::dogfood) async fn run<T, F, Fut>(
    command: Command,
    control: Control<'_>,
    exchange: F,
) -> Result<(ExitStatus, T), String>
where
    F: FnOnce(ChildStdin, ChildStdout, ChildStderr) -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    if cancelled(control.owner.map(|owner| &owner.cancellation)) || cancelled(control.cancellation)
    {
        return Err("configured helper cancelled before launch; no command ran".into());
    }
    let group = processkit::ProcessGroup::new()
        .map_err(|_| "configured helper containment could not be created")?;
    let receipt = control
        .owner
        .map(|owner| owner.cleanup.clone())
        .unwrap_or_default();
    receipt.started();
    let mut child = match group.spawn(command) {
        Ok(child) => child,
        Err(error) => {
            let cleanup = receipt.failed_spawn(&group, &error).await;
            let reason =
                "configured helper spawn failed; check executable, directory and permissions";
            return Err(match cleanup {
                Ok(()) => reason.to_owned(),
                Err(cleanup) => {
                    format!("{reason}; configured helper cleanup unconfirmed: {cleanup}")
                }
            });
        }
    };
    let leader = child.id();
    let collected = {
        let operation = async {
            let stdin = child
                .stdin
                .take()
                .ok_or("configured helper stdin is absent")?;
            let stdout = child
                .stdout
                .take()
                .ok_or("configured helper stdout is absent")?;
            let stderr = child
                .stderr
                .take()
                .ok_or("configured helper stderr is absent")?;
            let wait = async {
                root_exited(&mut child).await?;
                // A completed leader may leave descendants holding its pipes.
                // Keep the Unix leader unreaped until the final group census.
                group
                    .kill_all()
                    .map_err(|_| "configured helper descendant termination failed".to_owned())
            };
            let (output, ()) = tokio::try_join!(exchange(stdin, stdout, stderr), wait)?;
            Ok::<_, String>(output)
        };
        tokio::select! {
            result = tokio::time::timeout_at(control.deadline, operation) => result
                .map_err(|_| "configured helper timed out at its deadline".to_owned())
                .and_then(|result| result),
            _ = cancellation(control.owner.map(|owner| &owner.cancellation)) =>
                Err("configured helper cancelled".into()),
            _ = cancellation(control.cancellation) =>
                Err("configured helper cancelled".into()),
        }
    };
    // Even a pipe setup failure must reach cleanup, not return through `?`.
    drop(child.stdin.take());
    drop(child.stdout.take());
    drop(child.stderr.take());
    let terminated = group.kill_all().map_err(|_| {
        "configured helper termination failed; descendants may still be running".to_owned()
    });
    let confirmed = confirm(&group, leader, Instant::now() + CLEANUP_BOUND).await;
    let reaped = tokio::time::timeout(REAP_BOUND, child.wait())
        .await
        .map_err(|_| "configured helper cleanup could not confirm leader exit".to_owned())
        .and_then(|result| result.map_err(|_| "configured helper leader reap failed".to_owned()));
    let cleanup = terminated
        .and(confirmed)
        .and(reaped.as_ref().map(|_| ()).map_err(Clone::clone));
    receipt.settled(cleanup.clone());
    cleanup?;
    Ok((reaped?, collected?))
}

fn cancelled(signal: Option<&ToolCancellation>) -> bool {
    signal.is_some_and(|signal| signal.cancelled_by().is_some())
}

async fn cancellation(signal: Option<&ToolCancellation>) {
    match signal {
        Some(signal) => {
            let mut signal = signal.clone();
            if signal.cancelled_by().is_none() {
                // A closed cancellation channel also ends the helper.
                let _source = signal.changed().await;
            }
        }
        None => std::future::pending().await,
    }
}

async fn root_exited(child: &mut Child) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        use nix::sys::wait::{waitid, Id, WaitPidFlag, WaitStatus};
        let pid = child
            .id()
            .and_then(|pid| i32::try_from(pid).ok())
            .ok_or("configured helper leader identity is unavailable")?;
        loop {
            match waitid(
                Id::Pid(nix::unistd::Pid::from_raw(pid)),
                WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT,
            )
            .map_err(|error| format!("configured helper exit observation failed: {error}"))?
            {
                WaitStatus::Exited(_, _) | WaitStatus::Signaled(_, _, _) => return Ok(()),
                WaitStatus::StillAlive => tokio::time::sleep(Duration::from_millis(10)).await,
                _ => return Err("configured helper exit observation was not terminal".into()),
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        crate::dogfood::tools::macos_cleanup::root_exited(child)
            .await
            .map_err(|error| format!("configured helper exit observation failed: {error}"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    child
        .wait()
        .await
        .map(|_| ())
        .map_err(|_| "configured helper leader wait failed".into())
}

async fn confirm(
    group: &processkit::ProcessGroup,
    leader: Option<u32>,
    deadline: Instant,
) -> Result<(), String> {
    loop {
        let running = match group.mechanism() {
            processkit::Mechanism::CgroupV2 | processkit::Mechanism::ProcessReaper => !group
                .members()
                .map_err(|error| format!("configured helper tree census failed: {error}"))?
                .is_empty(),
            #[cfg(windows)]
            processkit::Mechanism::JobObject => {
                // This API queries the entire job, not just spawned leaders.
                !group
                    .members_info()
                    .map_err(|error| format!("configured helper job census failed: {error}"))?
                    .is_empty()
            }
            #[cfg(target_os = "linux")]
            processkit::Mechanism::ProcessGroup => {
                crate::dogfood::tools::group_running(
                    leader.ok_or("configured helper group leader identity is unavailable")?,
                    deadline,
                )
                .await?
            }
            #[cfg(target_os = "macos")]
            processkit::Mechanism::ProcessGroup => {
                crate::dogfood::tools::macos_cleanup::group_running(
                    leader.ok_or("configured helper group leader identity is unavailable")?,
                    deadline,
                )
                .await?
            }
            _ => {
                let _ = leader;
                return Err("configured helper cleanup has no whole-tree exit witness on this platform; cleanup is unconfirmed".into());
            }
        };
        if !running {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(
                "configured helper descendants did not exit within the cleanup bound".into(),
            );
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
