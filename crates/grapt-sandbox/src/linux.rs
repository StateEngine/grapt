//! Copyright 2025 OpenAI.
//! Apache-2.0. Modified: bounded shared policy and existing terminal ownership.
use super::{ContainedShell, ContainedShellSpawner, NetworkAccess, ShellRequest, SpawnFailure};
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::Command;

mod interrupt;
pub use interrupt::LinuxInterrupts;
mod mounts;
mod piped;
pub use piped::PipedCommand;
mod seccomp;
mod spawn;
mod targets;

const READY: &[u8] = b"GRAPT_BWRAP_READY\n";
pub const INNER_ARGUMENT: &str = "--grapt-linux-sandbox-exec";

/// The helper is the installed product binary, with `inner_main` dispatched
/// before its runtime or configuration starts. The system bwrap path is trusted
/// launcher configuration, never a program selected by the command environment.
#[derive(Debug)]
pub struct Bwrap {
    helper: PathBuf,
    executable: PathBuf,
}

impl Bwrap {
    pub fn new(helper: PathBuf) -> Self {
        Self {
            helper,
            executable: "/usr/bin/bwrap".into(),
        }
    }

    /// Explicit launcher injection, useful for distributions and unavailable
    /// backend checks. Repository configuration must not select this program.
    pub fn with_executable(mut self, executable: PathBuf) -> Self {
        self.executable = executable;
        self
    }

    /// A host-owned directory mounted read-only in every contained command.
    /// The product places private policy sockets here so a command with temp
    /// write authority cannot replace the socket before its helper connects.
    pub fn ipc_directory() -> Result<PathBuf, String> {
        targets::registry()
    }

    /// After its helper has been reaped, the existing process owner also
    /// releases temporary targets left by a killed helper. Active concurrent
    /// commands keep their shared leases, so their mounts remain protected.
    pub fn cleanup_targets(protected_paths: &[PathBuf]) -> Result<(), String> {
        targets::cleanup(protected_paths)
    }

    /// Recognize only this adapter's still-empty, inode-owned mount targets.
    /// This is metadata provenance, not filesystem access authority.
    pub fn synthetic_directory(path: &Path) -> Result<bool, String> {
        targets::synthetic_directory(path)
    }

    /// Called only in the single-threaded child after bubblewrap has installed
    /// its mounts and namespaces. The receipt precedes untrusted shell source.
    pub fn inner_main(arguments: &[String]) -> Option<Result<(), String>> {
        if arguments.first().is_none_or(|arg| arg != INNER_ARGUMENT) {
            return None;
        }
        Some((|| {
            let [_, network, source, leases @ ..] = arguments else {
                return Err("Linux sandbox helper arguments are malformed".into());
            };
            if source.is_empty() || source.len() > 65536 || source.contains('\0') {
                return Err("Linux sandbox command is malformed".into());
            }
            let network = match network.as_str() {
                "off" => false,
                "on" => true,
                _ => return Err("Linux sandbox network mode is malformed".into()),
            };
            seccomp::install(network)?;
            let _native_leases = targets::native_ready(leases)?;
            let mut interrupts = LinuxInterrupts::new().map_err(|e| e.to_string())?;
            std::io::stderr()
                .write_all(READY)
                .map_err(|e| format!("Linux sandbox readiness failed: {e}"))?;
            let mut command = Command::new("/bin/sh");
            command
                .args(["-c", source])
                .env_clear()
                .envs(std::env::vars_os());
            let mut child = spawn::inherited(&command)
                .map_err(|e| format!("Linux sandbox shell could not start: {e}"))?;
            let status = interrupts
                .wait_process(&mut child)
                .map_err(|e| format!("Linux sandbox shell observation failed: {e}"))?;
            std::process::exit(status);
        })())
    }
}

fn refused(reason: impl Into<String>, effects_possible: bool) -> SpawnFailure {
    SpawnFailure::for_backend("bwrap", reason, effects_possible)
}

impl ContainedShellSpawner for Bwrap {
    fn spawn(&self, request: &ShellRequest) -> Result<ContainedShell, SpawnFailure> {
        self.spawn_with_write_targets(request, &[])
    }

    #[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
    fn spawn_with_write_targets(
        &self,
        request: &ShellRequest,
        targets: &[crate::WriteTarget],
    ) -> Result<ContainedShell, SpawnFailure> {
        let mut request = request.clone();
        request.policy = request
            .policy
            .with_write_targets(targets)
            .map_err(|e| refused(e, false))?;
        request
            .validate()
            .map_err(|reason| refused(reason, false))?;
        if !self.executable.is_absolute() || !self.helper.is_absolute() {
            return Err(refused(
                "Linux sandbox needs absolute launcher and helper paths",
                false,
            ));
        }
        let mounts =
            mounts::Mounts::prepare(&request.policy, targets).map_err(|e| refused(e, false))?;
        let (mut info, info_write) = nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC)
            .map(|(read, write)| (std::fs::File::from(read), write))
            .map_err(|e| {
                refused(
                    format!("Linux sandbox identity pipe unavailable: {e}"),
                    false,
                )
            })?;
        let (startup_read, startup_write) = nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC)
            .map_err(|e| {
                refused(
                    format!("Linux sandbox startup pipe unavailable: {e}"),
                    false,
                )
            })?;
        let mut startup_write = std::fs::File::from(startup_write);
        let mut command = Command::new(&self.executable);
        command.args([
            "--die-with-parent",
            "--new-session",
            "--unshare-user",
            "--disable-userns",
            "--unshare-pid",
            "--as-pid-1",
            "--unshare-ipc",
        ]);
        use std::os::fd::AsRawFd;
        command
            .arg("--info-fd")
            .arg(info_write.as_raw_fd().to_string());
        command
            .arg("--block-fd")
            .arg(startup_read.as_raw_fd().to_string());
        if request.policy.network == NetworkAccess::Off {
            command.arg("--unshare-net");
        }
        command
            .args(&mounts.arguments)
            .args(["--cap-drop", "ALL", "--chdir"])
            .arg(&request.working_directory)
            .arg("--")
            .arg(&self.helper)
            .arg(INNER_ARGUMENT)
            .arg(if request.policy.network == NetworkAccess::Off {
                "off"
            } else {
                "on"
            })
            .arg(&request.command_line)
            .args(mounts.native_leases())
            .env_clear()
            .envs(
                request
                    .environment
                    .iter()
                    .filter_map(|entry| entry.split_once('='))
                    .filter(|(name, _)| {
                        !name.starts_with("LD_")
                            && !matches!(
                                *name,
                                "GLIBC_TUNABLES" | "GCONV_PATH" | "LOCPATH" | "BASH_ENV" | "ENV"
                            )
                    }),
            );
        let (mut process, mut pipe) = spawn::start(
            &command,
            targets,
            &[info_write.as_fd(), startup_read.as_fd()],
        )
        .map_err(|e| refused(format!("bubblewrap could not start: {e}"), false))?;
        drop(info_write);
        drop(startup_read);
        let started = (|| {
            let namespace = interrupt::namespace_process(&mut info, &process, request.timeout_ms)
                .map_err(|e| {
                refused(format!("Linux sandbox identity unavailable: {e}"), true)
            })?;
            // PID 1 remains blocked before it can execute untrusted source
            // until its kernel process handle has been authenticated.
            startup_write.write_all(&[1]).map_err(|e| {
                refused(format!("Linux sandbox startup release failed: {e}"), false)
            })?;
            drop(startup_write);
            let pending = ready(&mut pipe, request.timeout_ms)?;
            std::thread::Builder::new()
                .name("grapt-sandbox-stderr".into())
                .spawn(move || {
                    let mut destination = std::io::stderr();
                    destination.write_all(&pending)?;
                    std::io::copy(&mut pipe, &mut destination)?;
                    Ok(())
                })
                .map(|forwarder| (forwarder, namespace))
                .map_err(|e| refused(format!("Linux sandbox stderr forwarding failed: {e}"), true))
        })();
        match started {
            Ok((forwarder, namespace)) => Ok(ContainedShell {
                _write_targets: targets.to_vec(),
                child: Child {
                    process,
                    namespace,
                    forwarder: Some(forwarder),
                    mounts: Some(mounts),
                },
            }),
            Err(error) => {
                let _kill = process.kill();
                let _wait = process.wait();
                Err(error)
            }
        }
    }
}

fn ready(pipe: &mut std::fs::File, timeout_ms: u32) -> Result<Vec<u8>, SpawnFailure> {
    use nix::poll::{poll, PollFd, PollFlags};
    let mut bytes = Vec::new();
    // A fixed iteration budget bounds the whole startup without an ambient clock.
    let slice_ms = u16::try_from((timeout_ms.min(5000) / 16).max(1))
        .map_err(|_| refused("Linux sandbox startup bound is invalid", true))?;
    for _ in 0..16 {
        let available = {
            let mut descriptors = [PollFd::new(pipe.as_fd(), PollFlags::POLLIN)];
            match poll(&mut descriptors, slice_ms) {
                Err(nix::errno::Errno::EINTR) => continue,
                result => result.map_err(|e| {
                    refused(format!("bubblewrap startup observation failed: {e}"), true)
                })?,
            }
        };
        if available == 0 {
            continue;
        }
        let mut chunk = [0; 128];
        let count = match pipe.read(&mut chunk) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => {
                result.map_err(|e| refused(format!("bubblewrap startup read failed: {e}"), true))?
            }
        };
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(position) = bytes
            .windows(READY.len())
            .position(|window| window == READY)
        {
            bytes.drain(position..position + READY.len());
            return Ok(bytes);
        }
        if count == 0 {
            return Err(refused(
                format!(
                    "bubblewrap did not establish containment: {}",
                    String::from_utf8_lossy(&bytes).trim()
                ),
                false,
            ));
        }
    }
    Err(refused(
        format!(
            "bubblewrap startup timed out or exceeded its output bound: {}",
            String::from_utf8_lossy(&bytes).trim()
        ),
        true,
    ))
}

pub(super) struct Child {
    process: spawn::Process,
    namespace: std::os::fd::OwnedFd,
    // Stream forwarding preserves stderr without collecting another transcript.
    // It shares this child's lifetime; the caller still owns output limits and
    // process cancellation through the existing terminal/process supervisor.
    forwarder: Option<std::thread::JoinHandle<std::io::Result<()>>>,
    mounts: Option<mounts::Mounts>,
}

impl Child {
    fn interrupt(&mut self) -> std::io::Result<()> {
        match rustix::process::pidfd_send_signal(&self.namespace, rustix::process::Signal::INT) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
    pub fn wait(&mut self) -> std::io::Result<i32> {
        let status = self.process.wait()?;
        self.join_stderr()?;
        self.mounts.take();
        Ok(status)
    }
    pub fn try_wait(&mut self) -> std::io::Result<Option<i32>> {
        let status = self.process.try_wait()?;
        if status.is_some() {
            self.join_stderr()?;
            self.mounts.take();
        }
        Ok(status)
    }
    pub fn kill(&mut self) -> std::io::Result<()> {
        // Killing namespace PID 1 stops every contained descendant, including
        // commands that moved into another process group. Keep the monitor
        // alive to reap the namespace and release its native target leases.
        match rustix::process::pidfd_send_signal(&self.namespace, rustix::process::Signal::KILL) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn join_stderr(&mut self) -> std::io::Result<()> {
        if let Some(forwarder) = self.forwarder.take() {
            forwarder.join().map_err(|_| {
                std::io::Error::other("Linux sandbox stderr forwarder stopped unexpectedly")
            })??;
        }
        Ok(())
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if !matches!(self.process.try_wait(), Ok(Some(_))) {
            let _kill = self.kill();
            let _wait = self.process.wait();
        }
        let _joined = self.join_stderr();
    }
}
