//! macOS group cleanup while Control still reserves the unreaped leader PID.
//! Modified: bounded trusted ps census, group-only signals, and no successful cleanup
//! claim until a complete census contains no live member of the owned group.
use nix::errno::Errno;

const MAX_CENSUS_BYTES: usize = 1024 * 1024;
const MAX_CENSUS_ROWS: usize = 32_768;

fn parse(bytes: &[u8], group: i32) -> Result<bool, String> {
    if bytes.len() > MAX_CENSUS_BYTES {
        return Err("terminal group census exceeds 1 MiB".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "terminal group census is not UTF-8")?;
    let mut live = false;
    let mut leader = false;
    for (index, line) in text.lines().enumerate() {
        if index >= MAX_CENSUS_ROWS {
            return Err("terminal group census exceeds 32768 rows".into());
        }
        let mut fields = line.split_whitespace();
        let mut number = || {
            fields
                .next()
                .and_then(|field| field.parse::<i32>().ok())
                .filter(|number| *number >= 0)
                .ok_or("terminal group census has an invalid identity")
        };
        let pid = number()?;
        let pgid = number()?;
        let state = fields.next().ok_or("terminal group census has no state")?;
        if state.len() > 32 || fields.next().is_some() {
            return Err("terminal group census has malformed fields".into());
        }
        if pgid == group {
            leader |= pid == group;
            live |= !state.starts_with('Z');
        }
    }
    if !leader {
        return Err("terminal census did not contain the reserved group leader".into());
    }
    Ok(live)
}

fn cleanup(
    group: i32,
    mut signal_group: impl FnMut(i32) -> Result<(), Errno>,
    mut census: impl FnMut(i32) -> Result<bool, String>,
) -> Result<(), String> {
    if group <= 1 {
        return Err("terminal cleanup needs a private positive group".into());
    }
    // Each census has a half-second observation bound. The unreaped leader
    // reserves the group identity; individual member PIDs are never signalled.
    for _ in 0..4 {
        let denied = match signal_group(group) {
            Ok(()) => false,
            Err(Errno::EPERM) => true,
            Err(Errno::ESRCH) => return Ok(()),
            Err(error) => return Err(format!("terminal group cleanup failed: {error}")),
        };
        if !census(group)? {
            return Ok(());
        }
        if denied {
            return Err("terminal group cleanup denied while live members remain".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Err("terminal group still has live members after cleanup".into())
}

#[cfg(target_os = "macos")]
pub(super) fn cancel(group: i32) -> Result<(), String> {
    cleanup(
        group,
        |group| {
            nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(group),
                nix::sys::signal::Signal::SIGKILL,
            )
        },
        |group| parse(&census()?, group),
    )
}

#[cfg(target_os = "macos")]
fn census() -> Result<Vec<u8>, String> {
    use nix::fcntl::{fcntl, FcntlArg, OFlag};
    use std::io::{ErrorKind, Read};
    use std::process::{Child, Command, Stdio};
    struct Probe(Child);
    impl Drop for Probe {
        fn drop(&mut self) {
            if self.0.kill().is_ok() {
                let _result = self.0.wait();
            }
        }
    }
    // The probe owns only its trusted ps child, never a terminal member.
    // Read just PID, group and state; argv and environment are never inspected.
    let mut probe = Probe(
        Command::new("/bin/ps")
            .args(["-A", "-o", "pid=,pgid=,stat="])
            .current_dir("/")
            .env_clear()
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("terminal census unavailable: {error}"))?,
    );
    let mut output = probe.0.stdout.take().ok_or("terminal census has no pipe")?;
    let flags = fcntl(&output, FcntlArg::F_GETFL)
        .map_err(|error| format!("terminal census pipe unavailable: {error}"))?;
    fcntl(
        &output,
        FcntlArg::F_SETFL(OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK),
    )
    .map_err(|error| format!("terminal census pipe could not be bounded: {error}"))?;
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    let mut eof = false;
    for _ in 0..50 {
        for _ in 0..=MAX_CENSUS_BYTES / buffer.len() {
            match output.read(&mut buffer) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(count) => {
                    if bytes.len() + count > MAX_CENSUS_BYTES {
                        return Err("terminal group census exceeds 1 MiB".into());
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => (),
                Err(error) => return Err(format!("terminal census read failed: {error}")),
            }
        }
        if let Some(status) = probe
            .0
            .try_wait()
            .map_err(|error| format!("terminal census wait failed: {error}"))?
        {
            if !status.success() {
                return Err("terminal census command failed".into());
            }
            if eof {
                return Ok(bytes);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Err("terminal census exceeded its 500 ms observation bound".into())
}

/// Observe exit without releasing the leader's identity before the final census.
#[cfg(target_os = "macos")]
pub(in crate::dogfood) async fn root_exited(child: &tokio::process::Child) -> std::io::Result<()> {
    use rustix::process::{waitid, Pid, WaitId, WaitIdOptions};
    let pid = child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .and_then(Pid::from_raw)
        .ok_or_else(|| std::io::Error::other("command leader identity is unavailable"))?;
    loop {
        match waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )
        .map_err(std::io::Error::from)?
        {
            Some(status)
                if status.exit_status().is_some() || status.terminating_signal().is_some() =>
            {
                return Ok(());
            }
            Some(_) => {
                return Err(std::io::Error::other(
                    "command exit observation was not terminal",
                ));
            }
            None => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
        }
    }
}

/// The caller keeps the owned group leader unreaped until this census completes.
#[cfg(target_os = "macos")]
pub(in crate::dogfood) async fn group_running(
    group: u32,
    deadline: tokio::time::Instant,
) -> Result<bool, String> {
    let group = i32::try_from(group)
        .map_err(|_| "command process group exceeds platform identity bounds")?;
    if group <= 1 {
        return Err("command group observation requires a private positive group".into());
    }
    let bytes = census_async(deadline).await?;
    let running = parse(&bytes, group)?;
    if tokio::time::Instant::now() >= deadline {
        return Err("command group cleanup exceeded its observation deadline".into());
    }
    Ok(running)
}

#[cfg(target_os = "macos")]
async fn census_async(deadline: tokio::time::Instant) -> Result<Vec<u8>, String> {
    use std::process::Stdio;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;
    use tokio::process::Command;
    use tokio::time::{timeout, timeout_at, Instant};

    if Instant::now() >= deadline {
        return Err("command group cleanup exceeded its observation deadline".into());
    }
    // Fixed trusted executable and fields only: never inspect command content.
    let mut probe = Command::new("/bin/ps")
        .args(["-A", "-o", "pid=,pgid=,stat="])
        .current_dir("/")
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("command group census unavailable: {error}"))?;
    let observed = timeout_at(
        deadline.min(Instant::now() + Duration::from_millis(500)),
        async {
            let output = probe
                .stdout
                .take()
                .ok_or("command group census has no pipe")?;
            let mut bytes = Vec::new();
            output
                .take(MAX_CENSUS_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .await
                .map_err(|error| format!("command group census read failed: {error}"))?;
            if bytes.len() > MAX_CENSUS_BYTES {
                return Err("command group census exceeds 1 MiB".to_owned());
            }
            let status = probe
                .wait()
                .await
                .map_err(|error| format!("command group census wait failed: {error}"))?;
            if !status.success() {
                return Err("command group census command failed".to_owned());
            }
            Ok(bytes)
        },
    )
    .await
    .unwrap_or_else(|_| Err("command group census exceeded its observation bound".into()));
    match observed {
        Ok(bytes) => Ok(bytes),
        Err(reason) => {
            // Also reap a failed or timed-out probe; kill_on_drop is only a backstop.
            let killed = probe.start_kill();
            let reaped = timeout(Duration::from_millis(500), probe.wait()).await;
            match (killed, reaped) {
                (Ok(()), Ok(Ok(_))) => Err(reason),
                (killed, reaped) => Err(format!(
                    "{reason}; census probe cleanup unconfirmed: kill={killed:?}, reap={reaped:?}"
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denied_group_cleanup_requires_a_complete_census_without_live_members() -> Result<(), String>
    {
        cleanup(10, |_| Err(Errno::EPERM), |_| parse(b"10 10 Z\n", 10))?;
        let mut snapshots = 0;
        let mut signals = 0;
        cleanup(
            10,
            |group| {
                assert_eq!(group, 10, "only the reserved group may be signalled");
                signals += 1;
                Ok(())
            },
            |_| {
                snapshots += 1;
                parse(
                    if snapshots == 1 {
                        b"10 10 Z\n11 10 S\n"
                    } else {
                        b"10 10 Z\n"
                    },
                    10,
                )
            },
        )?;
        assert_eq!(signals, 2);
        assert!(cleanup(10, |_| Err(Errno::EPERM), |_| Err("unreadable".into())).is_err());
        assert!(cleanup(
            10,
            |_| Err(Errno::EPERM),
            |_| parse(b"10 10 Z\n11 10 S\n", 10)
        )
        .is_err());
        assert!(cleanup(10, |_| Ok(()), |_| parse(b"10 10 Z\n11 10 S\n", 10)).is_err());
        assert!(parse(b"11 10 S\n", 10).is_err());
        assert!(parse(b"10 10\n", 10).is_err());
        Ok(())
    }
}
