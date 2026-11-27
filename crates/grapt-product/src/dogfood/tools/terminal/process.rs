//! OS handles behind the session terminal registry.
//!
//! Modified by Grapt: safe adapters, reserved leader PID through group cleanup,
//! filtered parent environment and bounded resource ownership.
use super::*;
#[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
#[path = "macos_cleanup.rs"]
pub(in crate::dogfood) mod macos_cleanup;
#[cfg(windows)]
#[path = "windows_process.rs"]
mod windows;
/// OS creation APIs can fail after a child has run but before handles return.
#[derive(Debug)]
pub(in crate::dogfood::tools) struct SpawnFailure {
    pub detail: String,
    pub effects_uncertain: bool,
    #[cfg(windows)]
    pub diagnostic: Option<serde_json::Value>,
}
impl SpawnFailure {
    #[cfg(any(windows, unix, test))]
    pub fn uncertain(detail: String) -> Self {
        Self {
            detail,
            effects_uncertain: true,
            #[cfg(windows)]
            diagnostic: None,
        }
    }
}
impl From<String> for SpawnFailure {
    fn from(detail: String) -> Self {
        Self {
            detail,
            effects_uncertain: false,
            #[cfg(windows)]
            diagnostic: None,
        }
    }
}
impl From<&str> for SpawnFailure {
    fn from(detail: &str) -> Self {
        detail.to_owned().into()
    }
}
impl std::fmt::Display for SpawnFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}
impl std::error::Error for SpawnFailure {}

#[cfg(not(windows))]
use std::io::{Read, Write};
#[cfg(not(windows))]
use std::process::{Command, Stdio};
#[cfg(windows)]
pub(in crate::dogfood) use windows::{launch_path, shell_cwd};
#[cfg(windows)]
pub(super) use windows::{spawn, Control};

#[cfg(not(windows))]
pub(super) struct Spawned {
    pub control: Arc<Mutex<Control>>,
    pub readers: Vec<Box<dyn Read + Send>>,
    pub writer: Box<dyn Write + Send>,
    #[cfg(target_os = "linux")]
    pub enforcement: RealizedEnforcement,
    armed: bool,
}

#[cfg(not(windows))]
impl Drop for Spawned {
    fn drop(&mut self) {
        if self.armed {
            let _result = lock(&self.control).cancel();
        }
    }
}

#[cfg(not(windows))]
pub(super) struct Control {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    pid: u32,
    signallable: bool,
    #[cfg(target_os = "linux")]
    sandbox: Option<super::super::sandbox::Plan>,
    #[cfg(target_os = "linux")]
    settling: Option<(i32, Option<String>, tokio::time::Instant)>,
    #[cfg(target_os = "linux")]
    group_cleanup: Option<GroupCleanup>,
}

#[cfg(target_os = "linux")]
struct GroupCleanup {
    deadline: tokio::time::Instant,
    signal_error: Option<String>,
    observation: Option<super::linux_cleanup::Observation>,
}

#[cfg(target_os = "linux")]
impl GroupCleanup {
    fn poll(&mut self, pid: u32) -> Result<Option<bool>, String> {
        if self.observation.is_none() {
            self.observation = super::linux_cleanup::start(pid, self.deadline)?;
        }
        let Some(observation) = self.observation.as_mut() else {
            return Ok(None);
        };
        let result = observation.poll(self.deadline);
        if !matches!(&result, Ok(None)) {
            self.observation = None;
        }
        result
    }
}

#[cfg(not(windows))]
impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalControl")
            .field("signallable", &self.signallable)
            .finish_non_exhaustive()
    }
}

#[cfg(not(windows))]
impl Control {
    pub fn cancel(&mut self) -> Result<(), String> {
        if !self.signallable {
            return Ok(());
        }
        #[cfg(unix)]
        {
            let pid =
                i32::try_from(self.pid).map_err(|_| "terminal PID exceeds platform bounds")?;
            #[cfg(target_os = "macos")]
            {
                macos_cleanup::cancel(pid)
            }
            #[cfg(not(target_os = "macos"))]
            {
                let signal = nix::sys::signal::Signal::SIGKILL;
                #[cfg(target_os = "linux")]
                let signal = if self.sandbox.is_some() {
                    // The trusted helper owns a separate namespace session. Let it
                    // stop that namespace and reap its monitor before it exits.
                    nix::sys::signal::Signal::SIGTERM
                } else {
                    signal
                };
                match nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), signal) {
                    Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
                    Err(error) => Err(format!("terminal group cleanup failed: {error}")),
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn poll_targets(&mut self) -> Result<Option<(i32, Option<String>)>, String> {
        let Some((code, prior, deadline)) = self.settling.take() else {
            return Ok(None);
        };
        let result = self
            .sandbox
            .as_ref()
            .map(super::super::sandbox::posix::cleanup)
            .transpose();
        match result {
            Ok(_) => Ok(Some((code, prior))),
            Err(reason) if tokio::time::Instant::now() >= deadline => Ok(Some((
                code,
                Some(prior.map_or(reason.clone(), |first| format!("{first}; {reason}"))),
            ))),
            Err(_) => {
                self.settling = Some((code, prior, deadline));
                Ok(None)
            }
        }
    }

    pub fn interrupt(&mut self) -> Result<(), String> {
        if !self.signallable {
            return Err("terminal process has already exited".into());
        }
        #[cfg(unix)]
        {
            let pid =
                i32::try_from(self.pid).map_err(|_| "terminal PID exceeds platform bounds")?;
            nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGINT,
            )
            .map_err(|error| format!("terminal interrupt failed: {error}"))
        }
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn hold_stdout_for_test(&self) -> Result<std::fs::File, String> {
        if !self.signallable {
            return Err("fixture child already exited".into());
        }
        // The control lock and unreaped child reserve this PID while procfs
        // duplicates its pipe endpoint. No extra child or raw-fd conversion.
        std::fs::OpenOptions::new()
            .write(true)
            .open(format!("/proc/{}/fd/1", self.pid))
            .map_err(|error| format!("fixture stdout duplicate failed: {error}"))
    }

    /// Linux and macOS observe a zombie without reaping it. While holding this lock,
    /// cleanup signals its still-reserved process group. Native Linux groups
    /// and private sessions remain reserved until a bounded blocking census
    /// confirms no live members.
    /// Signalling is disabled before releasing the leader PID by reaping it.
    #[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
    pub fn poll(&mut self) -> Result<Option<(i32, Option<String>)>, String> {
        #[cfg(target_os = "linux")]
        if self.settling.is_some() {
            return self.poll_targets();
        }
        if !self.signallable {
            return Ok(None);
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let pid =
                i32::try_from(self.pid).map_err(|_| "terminal PID exceeds platform bounds")?;
            #[cfg(target_os = "linux")]
            let code = {
                use nix::sys::wait::{waitid, Id, WaitPidFlag, WaitStatus};
                let status = match waitid(
                    Id::Pid(nix::unistd::Pid::from_raw(pid)),
                    WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT,
                ) {
                    Ok(status) => status,
                    Err(nix::errno::Errno::ECHILD) => {
                        self.signallable = false;
                        return Err(
                            "terminal child was reaped outside its owner; cleanup cannot be proved"
                                .into(),
                        );
                    }
                    Err(error) => return Err(format!("terminal exit observation failed: {error}")),
                };
                if matches!(status, WaitStatus::StillAlive) {
                    return Ok(None);
                }
                match status {
                    WaitStatus::Exited(_, code) => code,
                    WaitStatus::Signaled(_, signal, _) => 128 + signal as i32,
                    _ => return Err("terminal exit observation was not terminal".into()),
                }
            };
            #[cfg(target_os = "macos")]
            let code = {
                use rustix::process::{waitid, WaitId, WaitIdOptions};
                let pid = rustix::process::Pid::from_raw(pid)
                    .ok_or("terminal PID is not a positive native identity")?;
                let status = match waitid(
                    WaitId::Pid(pid),
                    WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
                ) {
                    Ok(Some(status)) => status,
                    Ok(None) => return Ok(None),
                    Err(rustix::io::Errno::CHILD) => {
                        self.signallable = false;
                        return Err("terminal child was reaped outside its owner; cleanup cannot be proved".into());
                    }
                    Err(error) => return Err(format!(
                        "terminal descendant cleanup is unavailable: exit observation failed: {error}"
                    )),
                };
                status
                    .exit_status()
                    .or_else(|| status.terminating_signal().map(|signal| 128 + signal))
                    .ok_or("terminal exit observation was not terminal")?
            };
            let cleanup = self.cancel().err();
            #[cfg(target_os = "linux")]
            let cleanup = if self.sandbox.is_none() {
                let group = self.group_cleanup.get_or_insert_with(|| GroupCleanup {
                    deadline: tokio::time::Instant::now() + STOP_SETTLE_BOUND,
                    signal_error: None,
                    observation: None,
                });
                if group.signal_error.is_none() {
                    group.signal_error = cleanup;
                }
                let running = group
                    .poll(self.pid)
                    .map_err(|reason| match &group.signal_error {
                        Some(signal) => format!("{signal}; {reason}"),
                        None => reason,
                    })?;
                if running != Some(false) {
                    return Ok(None);
                }
                group.signal_error.clone()
            } else {
                cleanup
            };
            self.signallable = false;
            self.master.take();
            self.child
                .wait()
                .map_err(|error| format!("terminal reap failed: {error}"))?;
            #[cfg(target_os = "linux")]
            if self.sandbox.is_some() {
                self.settling = Some((
                    code,
                    cleanup,
                    tokio::time::Instant::now() + STOP_SETTLE_BOUND,
                ));
                return self.poll_targets();
            }
            Ok(Some((code, cleanup)))
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let status = self
                .child
                .try_wait()
                .map_err(|error| format!("terminal exit observation failed: {error}"))?;
            let Some(status) = status else {
                return Ok(None);
            };
            let cleanup =
                Some("descendant cleanup after exit is unavailable on this platform".to_owned());
            self.signallable = false;
            self.master.take();
            let code = i32::try_from(status.exit_code())
                .map_err(|_| "terminal exit code exceeds bounds")?;
            Ok(Some((code, cleanup)))
        }
    }
}

#[cfg(not(windows))]
impl Drop for Control {
    fn drop(&mut self) {
        // Revoke census signals before any child-handle teardown can reap.
        #[cfg(target_os = "linux")]
        self.group_cleanup.take();
        // The watcher normally reaps first. If startup unwinds, at least stop
        // the owned process tree; never signal after a successful reap.
        let _result = self.cancel();
    }
}

#[cfg(not(windows))]
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
pub(super) fn spawn(spec: &ExecSpec) -> Result<Spawned, SpawnFailure> {
    #[cfg(not(target_os = "linux"))]
    if spec.environment.sandbox().is_some() {
        let enforcement =
            super::super::sandbox::degraded("MXC native process containment requires Windows");
        return Err(super::super::sandbox::refusal(
            &enforcement,
            "sandbox unavailable; no command ran",
        )
        .into());
    }

    #[cfg(not(windows))]
    let _cmd_shell = spec.cmd_shell;
    let actual_cwd = std::fs::canonicalize(&spec.cwd)
        .map_err(|error| format!("terminal workdir is unreadable: {error}"))?;
    if actual_cwd != spec.cwd || !actual_cwd.is_dir() {
        return Err("terminal workdir changed after authorization".into());
    }
    let program = spec.program.clone();
    let arguments: Vec<std::ffi::OsString> = spec.arguments.iter().map(Into::into).collect();
    #[cfg(target_os = "linux")]
    let (program, arguments, pending) = if let Some(plan) = spec.environment.sandbox() {
        use super::super::sandbox::posix::{command_line, Pending};
        let line = command_line(program.as_os_str(), &arguments)?;
        let pending = Pending::prepare(
            plan,
            line,
            spec.cwd.clone(),
            &spec.environment,
            spec.timeout_ms,
        )?;
        let helper =
            std::env::current_exe().map_err(|e| format!("sandbox helper unavailable: {e}"))?;
        (helper, pending.arguments().into(), Some(pending))
    } else {
        (program, arguments, None)
    };
    let mut readers: Vec<Box<dyn Read + Send>> = Vec::new();
    let (child, master, writer): (_, _, Box<dyn Write + Send>) = if spec.tty {
        let pair = portable_pty::native_pty_system()
            .openpty(portable_pty::PtySize {
                rows: 36,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| format!("terminal allocation failed: {error}"))?;
        let mut command = portable_pty::CommandBuilder::new(&program);
        command.cwd(&spec.cwd);
        command.env_clear();
        for (name, value) in spec
            .environment
            .iter()
            .filter(|_| spec.environment.sandbox().is_none())
        {
            command.env(name, value);
        }
        command.args(&arguments);
        // Acquire fallible I/O handles before starting a process.
        readers.push(
            pair.master
                .try_clone_reader()
                .map_err(|error| error.to_string())?,
        );
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| error.to_string())?;
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| format!("terminal spawn failed: {error}"))?;
        drop(pair.slave);
        (child, Some(pair.master), writer)
    } else {
        let mut command = Command::new(&program);
        command
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(
                spec.environment
                    .iter()
                    .filter(|_| spec.environment.sandbox().is_none()),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.args(&arguments).process_group(0);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("terminal spawn failed: {error}"))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        match (stdin, stdout, stderr) {
            (Some(stdin), Some(stdout), Some(stderr)) => {
                readers.push(Box::new(stdout));
                readers.push(Box::new(stderr));
                (
                    Box::new(child) as Box<dyn portable_pty::Child + Send + Sync>,
                    None,
                    Box::new(stdin) as Box<dyn Write + Send>,
                )
            }
            _ => {
                let _kill = child.kill();
                let _wait = child.wait();
                return Err(SpawnFailure::uncertain(
                    "terminal pipes were absent after spawn".into(),
                ));
            }
        }
    };
    let control = Control {
        pid: child.process_id().ok_or("terminal child has no PID")?,
        child,
        master,
        signallable: true,
        #[cfg(target_os = "linux")]
        sandbox: pending.as_ref().map(|pending| pending.plan().clone()),
        #[cfg(target_os = "linux")]
        settling: None,
        #[cfg(target_os = "linux")]
        group_cleanup: None,
    };
    #[cfg(target_os = "linux")]
    let mut control = control;
    #[cfg(target_os = "linux")]
    let enforcement = if let Some(pending) = pending {
        let runtime = tokio::runtime::Handle::current();
        match runtime.block_on(pending.finish(control.pid)) {
            Ok(enforcement) => enforcement,
            Err(reason) => {
                let stopped = control.cancel();
                let reaped = runtime.block_on(tokio::time::timeout(STOP_SETTLE_BOUND, async {
                    loop {
                        if let Some((_, cleanup)) = control.poll()? {
                            return cleanup.map_or(Ok(()), Err);
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }));
                let cleanup = match (stopped, reaped) {
                    (Err(error), _) | (_, Ok(Err(error))) => Some(error),
                    (_, Err(_)) => Some("sandbox startup cleanup was not confirmed".into()),
                    _ => None,
                };
                return Err(SpawnFailure {
                    effects_uncertain: serde_json::from_str::<serde_json::Value>(&reason)
                        .ok()
                        .is_none_or(|value| value["ran"] != false),
                    detail: cleanup.map_or(reason.clone(), |cleanup| {
                        super::super::sandbox::posix::failed_cleanup(&reason, &cleanup)
                    }),
                });
            }
        }
    } else {
        RealizedEnforcement::HarnessOnly
    };
    Ok(Spawned {
        control: Arc::new(Mutex::new(control)),
        readers,
        writer,
        #[cfg(target_os = "linux")]
        enforcement,
        armed: true,
    })
}

#[cfg(not(windows))]
impl Spawned {
    pub fn disarm(&mut self) {
        self.armed = false;
    }
}
