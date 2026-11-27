//! Forward Ctrl-C and cancellation through the trusted namespace owner.
//! (Copyright 2025 OpenAI, Apache-2.0). Safe signal-hook and pidfd replace raw handlers.
use super::spawn::Process;
use crate::ContainedShell;
use nix::poll::{poll, PollFd, PollFlags};
use signal_hook::iterator::Signals;
use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};

/// Install in the dedicated launcher before it spawns children or forwarding
/// threads. Caught dispositions also allow SIGINT to reach namespace PID 1.
#[derive(Debug)]
pub struct LinuxInterrupts {
    signals: Signals,
}

impl LinuxInterrupts {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            signals: Signals::new([signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM])?,
        })
    }

    fn pending(&mut self) -> Option<i32> {
        let mut interrupted = None;
        for signal in self.signals.pending() {
            // A cancellation takes precedence over a simultaneous Ctrl-C.
            if signal == signal_hook::consts::SIGTERM {
                return Some(signal);
            }
            interrupted = Some(signal);
        }
        if interrupted.is_none() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        interrupted
    }

    pub fn wait(&mut self, child: &mut ContainedShell) -> io::Result<i32> {
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            match self.pending() {
                Some(signal_hook::consts::SIGTERM) => child.kill()?,
                Some(_) => child.child.interrupt()?,
                None => {}
            }
        }
    }

    pub(super) fn wait_process(&mut self, child: &mut Process) -> io::Result<i32> {
        loop {
            if let Some(status) = child.try_wait_init()? {
                return Ok(status);
            }
            match self.pending() {
                Some(signal_hook::consts::SIGTERM) => child.kill()?,
                Some(_) => child.interrupt()?,
                None => {}
            }
        }
    }
}

fn bounded_file(path: impl AsRef<std::path::Path>) -> io::Result<String> {
    let mut text = String::new();
    std::fs::File::open(path)?
        .take(8193)
        .read_to_string(&mut text)?;
    if text.len() > 8192 {
        return Err(io::Error::other(
            "Sandbox process identity exceeds its bound",
        ));
    }
    Ok(text)
}

/// Authenticate the namespace's trusted PID 1 against our still-owned monitor.
/// Recheck the pidfd after procfs: a dead/reused numeric PID cannot be adopted.
pub(super) fn namespace_process(
    info: &mut std::fs::File,
    monitor: &Process,
    timeout_ms: u32,
) -> io::Result<OwnedFd> {
    let mut bytes = Vec::new();
    let slice = u16::try_from((timeout_ms.min(5000) / 16).max(1))
        .map_err(|_| io::Error::other("Sandbox startup timeout is invalid"))?;
    let mut complete = false;
    for _ in 0..16 {
        let mut descriptors = [PollFd::new(info.as_fd(), PollFlags::POLLIN)];
        let available = match poll(&mut descriptors, slice) {
            Err(nix::errno::Errno::EINTR) => continue,
            result => result?,
        };
        if available == 0 {
            continue;
        }
        let mut chunk = [0; 256];
        let count = match info.read(&mut chunk) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            complete = true;
            break;
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    if !complete {
        return Err(io::Error::other(
            "Sandbox namespace information timed out or exceeds its bound",
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    let pid = value["child-pid"]
        .as_u64()
        .and_then(|pid| i32::try_from(pid).ok())
        .and_then(rustix::process::Pid::from_raw)
        .ok_or_else(|| io::Error::other("Sandbox namespace process is invalid"))?;
    let handle = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())?;
    let status = bounded_file(format!("/proc/{}/status", pid.as_raw_nonzero()))?;
    let expected = monitor
        .id()
        .ok_or_else(|| io::Error::other("Sandbox monitor exited"))?;
    if !status.lines().any(|line| {
        line.strip_prefix("PPid:")
            .and_then(|value| value.trim().parse::<i32>().ok())
            == Some(expected.as_raw())
    }) {
        return Err(io::Error::other(
            "Sandbox namespace process is not owned by its monitor",
        ));
    }
    let fdinfo = bounded_file(format!("/proc/self/fdinfo/{}", handle.as_raw_fd()))?;
    if !fdinfo.lines().any(|line| {
        line.strip_prefix("Pid:")
            .and_then(|value| value.trim().parse::<i32>().ok())
            == Some(pid.as_raw_nonzero().get())
    }) {
        return Err(io::Error::other(
            "Sandbox namespace process exited during adoption",
        ));
    }
    Ok(handle)
}
