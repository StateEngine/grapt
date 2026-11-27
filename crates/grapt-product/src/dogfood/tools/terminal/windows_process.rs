//! Safe Windows terminal I/O behind the existing session registry.
//! Copyright 2025 OpenAI.
//! Apache-2.0; Modified by Grapt: safe native I/O and bounded registry ownership.
//! Both modes run inside a cleanup job whose job-wide exit is confirmed before
//! cleanup is reported complete: pipes directly, a pseudoconsole through the
//! terminal shim. Process cleanup is separate from the security sandbox.
use super::super::stop::{Root, Step, Stop, StopKind, CONFIRM_BOUND, REPORT_GRACE};
use super::super::{shim_protocol, windows_job, windows_shim};
use super::*;
use conpty_oxide::{blocking, ConPtyBackend, PtyController, SessionOptions, Size};
use shim_protocol::{Evidence, ShellLaunch};
use std::io::{Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf, Prefix};
use windows_job::ConfirmedJob;
use windows_spawn::{Command, CreationFlags, Stdio};

pub(in super::super) struct Spawned {
    pub control: Arc<Mutex<Control>>,
    pub readers: Vec<Box<dyn Read + Send>>,
    pub writer: Box<dyn Write + Send>,
    pub enforcement: RealizedEnforcement,
    armed: bool,
}
impl Drop for Spawned {
    fn drop(&mut self) {
        if self.armed {
            let _result = lock(&self.control).cancel();
        }
    }
}
impl Spawned {
    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

enum Child {
    Terminal {
        child: blocking::Child,
        controller: Option<PtyController>,
        link: windows_shim::Link,
    },
    Pipes {
        child: windows_spawn::Child,
        job: ConfirmedJob,
    },
}
/// A root exit whose job-wide cleanup is still being confirmed.
struct Settling {
    code: u32,
    deadline: tokio::time::Instant,
    failure: Option<String>,
}
pub(in super::super) struct Control {
    child: Child,
    signallable: bool,
    settling: Option<Settling>,
    stop: Stop,
}
impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalControl")
            .field("signallable", &self.signallable)
            .finish_non_exhaustive()
    }
}
impl Control {
    /// PTY commands report the diagnostic from their owning console shim.
    pub fn finish_msys_observation(&self) -> Option<bool> {
        match &self.child {
            Child::Pipes { job, .. } => {
                job.stop_msys();
                Some(job.observed_msys())
            }
            Child::Terminal { .. } => None,
        }
    }

    /// Stop the root within the shared bounds (see `stop`): the shim stops
    /// and confirms the shell's job itself; if it cannot be asked or does not
    /// finish in time the pseudoconsole is killed and cleanup stays
    /// unconfirmed.
    pub fn cancel(&mut self) -> Result<(), String> {
        if !self.signallable || self.settling.is_some() {
            return Ok(());
        }
        let now = tokio::time::Instant::now();
        self.stop
            .request(&mut self.child, StopKind::Terminate, now)
            .map(|_| ())
    }
    /// A console Ctrl-C was written but the command is still running: an
    /// ancestor may have disabled console Ctrl-C, which every descendant
    /// inherits. End the job with the Ctrl-C exit status instead, within the
    /// same bounds as a stop. `Ok(false)` when the root had already exited.
    pub fn force_interrupt(&mut self) -> Result<bool, String> {
        if !self.signallable || self.settling.is_some() {
            return Ok(false);
        }
        let now = tokio::time::Instant::now();
        self.stop.request(&mut self.child, StopKind::Interrupt, now)
    }
    pub fn interrupt(&mut self) -> Result<(), String> {
        if !self.signallable {
            return Err("terminal process has already exited".into());
        }
        match &self.child {
            Child::Terminal { .. } => Err("send terminal control characters through stdin".into()),
            // A pipe child has no console control channel. Keep cancellation
            // available by terminating only its owned job, using Windows' native
            // control-C exit code. The tool description discloses this fallback.
            Child::Pipes { job, .. } => job
                .terminate(0xC000_013A)
                .map_err(|error| format!("terminal job interruption failed: {error}")),
        }
    }
    /// The root's exit and cleanup outcome, once job-wide exit is confirmed
    /// or its bound has expired. Until then the exit is withheld, so the
    /// terminal's capacity stays reserved.
    pub fn poll(&mut self) -> Result<Option<(i32, Option<String>)>, String> {
        if !self.signallable {
            return Ok(None);
        }
        let now = tokio::time::Instant::now();
        if self.settling.is_none() {
            let code = match self.stop.step(&mut self.child, now) {
                Step::Running => return Ok(None),
                Step::Exited(code) => code,
                // The supervisor settles this as a failure: cleanup stays
                // unconfirmed and the terminal keeps its capacity.
                Step::Failed(reason) => return Err(reason),
            };
            self.settling = Some(match &self.child {
                // The root's exit ends the command; its descendants stop now.
                Child::Pipes { job, .. } => Settling {
                    code,
                    deadline: now + CONFIRM_BOUND,
                    failure: job.terminate(1).err(),
                },
                Child::Terminal { .. } => Settling {
                    code,
                    deadline: now + REPORT_GRACE,
                    failure: None,
                },
            });
        }
        let Some(settling) = self.settling.as_ref() else {
            return Ok(None);
        };
        let evidence = match (&settling.failure, &self.child) {
            (Some(failure), _) => Evidence::Unconfirmed(failure.clone()),
            (None, Child::Pipes { job, .. }) => match job.exited() {
                Ok(true) => Evidence::Confirmed,
                Ok(false) => Evidence::Pending,
                Err(error) => Evidence::Unconfirmed(error),
            },
            (None, Child::Terminal { link, .. }) => match link.heard() {
                windows_shim::Heard::Nothing => Evidence::Pending,
                windows_shim::Heard::Report(report) => {
                    shim_protocol::report_evidence(&report, settling.code)
                }
                windows_shim::Heard::Closed(reason) => Evidence::Unconfirmed(reason),
            },
        };
        let expired = now >= settling.deadline;
        let Some(cleanup) = shim_protocol::settle(evidence, expired, || match &self.child {
            Child::Pipes { job, .. } => windows_job::unconfirmed(job.active().unwrap_or(0)),
            Child::Terminal { .. } => {
                "terminal shim did not report job-wide exit; its capacity stays reserved".into()
            }
        }) else {
            return Ok(None);
        };
        let code = settling.code;
        self.signallable = false;
        if let Child::Terminal { controller, .. } = &mut self.child {
            controller.take();
        }
        // Windows uses all 32 bits for exit status. Preserve its bit pattern.
        Ok(Some((i32::from_ne_bytes(code.to_ne_bytes()), cleanup)))
    }
}

impl Root for Child {
    fn exit(&mut self) -> Result<Option<u32>, String> {
        let observed = |error: std::io::Error| format!("terminal exit observation failed: {error}");
        Ok(match self {
            Child::Terminal { child, .. } => child
                .try_wait()
                .map_err(|error| format!("terminal exit observation failed: {error}"))?
                .map(|status| u32::from_ne_bytes(status.code().to_ne_bytes())),
            Child::Pipes { child, .. } => match child.try_wait().map_err(observed)? {
                Some(status) => Some(u32::from_ne_bytes(
                    status
                        .code()
                        .ok_or("terminal exit has no code")?
                        .to_ne_bytes(),
                )),
                None => None,
            },
        })
    }
    fn request(&mut self, kind: StopKind) -> Result<bool, String> {
        Ok(match self {
            Child::Terminal { link, .. } => match kind {
                StopKind::Terminate => link.request_terminate(),
                StopKind::Interrupt => link.request_interrupt(),
            },
            Child::Pipes { job, .. } => {
                job.terminate(match kind {
                    StopKind::Terminate => 1,
                    StopKind::Interrupt => shim_protocol::CONTROL_C_EXIT,
                })?;
                true
            }
        })
    }
    fn kill(&mut self) -> Result<(), String> {
        match self {
            Child::Terminal { child, .. } => child
                .kill()
                .map_err(|error| format!("terminal kill failed: {error}")),
            Child::Pipes { job, .. } => job.terminate(1),
        }
    }
}
impl Drop for Control {
    fn drop(&mut self) {
        if self.signallable {
            let _killed = Root::kill(&mut self.child);
        }
    }
}

#[cfg_attr(windows, expect(clippy::too_many_lines, reason = "legacy"))]
pub(in super::super) fn spawn(
    spec: &ExecSpec,
    observed_ms: Option<u64>,
) -> Result<Spawned, SpawnFailure> {
    let actual_cwd = std::fs::canonicalize(&spec.cwd)
        .map_err(|error| format!("terminal workdir is unreadable: {error}"))?;
    if actual_cwd != spec.cwd || !actual_cwd.is_dir() {
        return Err("terminal workdir changed after authorization".into());
    }
    let cwd = shell_cwd(&spec.cwd, spec.cmd_shell)?;
    let program = launch_path(&spec.program)?;
    let mut launch = if spec.cmd_shell {
        ShellLaunch::Cmd {
            program: program.clone().into_os_string(),
            command: spec.command.clone().into(),
        }
    } else {
        ShellLaunch::Exec {
            program: program.clone().into_os_string(),
            arguments: spec.arguments.iter().map(Into::into).collect(),
        }
    };
    let pending = if let Some(plan) = spec.environment.sandbox() {
        use super::super::super::sandbox::windows::{quote, Pending};
        let line = match &launch {
            ShellLaunch::Cmd { program, command } => format!(
                "{} /D /S /C \"{}\"",
                quote(program)?,
                command.to_str().ok_or("sandbox source is not Unicode")?
            ),
            ShellLaunch::Exec { program, arguments } => {
                let mut line = quote(program)?;
                for argument in arguments {
                    line.push(' ');
                    line.push_str(&quote(argument)?);
                }
                line
            }
        };
        let pending =
            Pending::prepare(plan, line, cwd.clone(), &spec.environment, spec.timeout_ms)?;
        launch = ShellLaunch::Exec {
            program: std::env::current_exe()
                .map_err(|e| format!("sandbox helper unavailable: {e}"))?
                .into_os_string(),
            arguments: pending.arguments().into(),
        };
        Some(pending)
    } else {
        None
    };
    let (child, readers, writer): (_, Vec<Box<dyn Read + Send>>, Box<dyn Write + Send>) = if spec
        .tty
    {
        // This executable is the pseudoconsole root in shim mode. It is
        // resolved from the running image, never through PATH.
        let shim = std::env::current_exe()
            .map_err(|error| format!("terminal shim executable is unavailable: {error}"))?;
        let context = super::super::windows_diagnostics::Context::new(
            &shim,
            Some(&program),
            &cwd,
            observed_ms,
            "conpty-host",
        );
        let (pipe, link) = windows_shim::Link::listen(observed_ms, context.concurrent(), &context)?;
        let mut command = blocking::Command::new(&shim);
        command
            .current_dir(&cwd)
            .env_clear()
            .args(windows_shim::arguments(&pipe, &launch));
        for (name, value) in spec.environment.iter() {
            command.env(name, value);
        }
        // Select the OS implementation explicitly; never load an adjacent DLL.
        let backend = ConPtyBackend::system()
            .map_err(|error| format!("system ConPTY is unavailable: {error}"))?;
        let options = SessionOptions::new()
            .backend(backend)
            .size(Size::try_new(100, 36).map_err(|error| error.to_string())?);
        let capture = windows_spawn::diagnostics::Capture::new();
        let parts = command
                .spawn_with(options)
                .map_err(|error| {
                    let detail = match error.io_error() {
                        Some(source) => format!("terminal spawn failed: {error}: {source}"),
                        None => format!("terminal spawn failed: {error}"),
                    };
                    // Spawn includes post-creation watcher registration. The
                    // public error cannot prove that no command effects ran.
                    let mut failure = if error.kind() == conpty_oxide::ErrorKind::Spawn {
                        SpawnFailure::uncertain(detail)
                    } else {
                        SpawnFailure::from(detail)
                    };
                    failure.diagnostic = Some(context.failure(
                        capture.failure(),
                        error.io_error().and_then(std::io::Error::raw_os_error),
                        serde_json::json!({"owner":"conpty-oxide","policy":"kill_on_close","membership":"private", "console_state":if error.kind() == conpty_oxide::ErrorKind::CreateConsole { "setup_failed" } else { "created" }, "adapter_stage":format!("{:?}",error.kind())}),
                    ));
                    failure
                })?
                .into_parts();
        (
            Child::Terminal {
                child: parts.child,
                controller: Some(parts.controller),
                link,
            },
            vec![Box::new(parts.output)],
            Box::new(parts.input),
        )
    } else {
        let launched_program = match &launch {
            ShellLaunch::Cmd { program, .. } | ShellLaunch::Exec { program, .. } => program,
        };
        let context = super::super::windows_diagnostics::Context::new(
            Path::new(launched_program),
            Some(&program),
            &cwd,
            observed_ms,
            "pipes",
        );
        let mut job = ConfirmedJob::create_observed(&context)?;
        if spec.environment.sandbox().is_some() {
            job.observe_msys();
        }
        let mut command = Command::new(launched_program);
        command
            .current_dir(&cwd)
            .env_clear()
            .envs(spec.environment.iter())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match &launch {
            ShellLaunch::Cmd {
                command: source, ..
            } => {
                command
                    .args(["/D", "/S", "/C"])
                    .raw_arg(format!("\"{}\"", source.to_string_lossy()));
            }
            ShellLaunch::Exec { arguments, .. } => {
                command.args(arguments);
            }
        }
        // Created suspended in the observed kill-on-close job before it
        // resumes: ordinary descendants cannot start outside cleanup.
        let mut child = job.spawn_tracked(&mut command, CreationFlags::NO_WINDOW, &context)?;
        drop(context);
        match (child.stdin.take(), child.stdout.take(), child.stderr.take()) {
            (Some(stdin), Some(stdout), Some(stderr)) => (
                Child::Pipes { child, job },
                vec![Box::new(stdout), Box::new(stderr)],
                Box::new(stdin),
            ),
            _ => {
                let _kill = job.terminate(1);
                return Err(SpawnFailure::uncertain(
                    "terminal pipes were absent after spawn".into(),
                ));
            }
        }
    };
    let mut spawned = Spawned {
        enforcement: if spec.environment.msys_executables().is_empty() {
            RealizedEnforcement::HarnessOnly
        } else {
            spec.environment.intended()
        },
        control: Arc::new(Mutex::new(Control {
            child,
            signallable: true,
            settling: None,
            stop: Stop::default(),
        })),
        readers,
        writer,
        armed: true,
    };
    if let Some(pending) = pending {
        let expected_pid = {
            let control = lock(&spawned.control);
            match &control.child {
                Child::Terminal { child, .. } => child.id(),
                Child::Pipes { child, .. } => child.id(),
            }
        };
        spawned.enforcement = tokio::runtime::Handle::current()
            .block_on(pending.finish(expected_pid))
            .map_err(|reason| SpawnFailure {
                effects_uncertain: serde_json::from_str::<serde_json::Value>(&reason)
                    .ok()
                    .is_none_or(|value| value["ran"] != false),
                detail: reason,
                diagnostic: None,
            })?;
    }
    Ok(spawned)
}

// Both cmd cwd and .NET Framework PowerShell executable paths need ordinary
// Win32 spelling. Keep the approved canonical identity and verify the spelling
// resolves to it before either the PTY shim or pipe child receives it.
pub(in crate::dogfood) fn launch_path(canonical: &Path) -> Result<PathBuf, String> {
    let path = ordinary_path(canonical)?;
    match std::fs::canonicalize(&path) {
        Ok(resolved) if resolved == canonical => {}
        Err(error) if super::super::shell_path::execution_alias(&path, &error) => {}
        Err(error) => {
            return Err(format!(
                "terminal launch path {path:?} is unavailable: {error}"
            ))
        }
        Ok(_) => return Err("terminal launch spelling changed its authorized identity".into()),
    }
    Ok(path)
}

fn ordinary_path(canonical: &Path) -> Result<PathBuf, String> {
    if !canonical.is_absolute() {
        return Err("terminal launch path must be an absolute Windows path".into());
    }
    // DOS paths trim trailing dots/spaces. Rust may itself add a verbatim
    // prefix during filesystem operations, so refuse these spellings explicitly.
    if canonical.components().any(|part| matches!(part, Component::Normal(name)
        if name.encode_wide().last().is_some_and(|last| last == u16::from(b'.') || last == u16::from(b' ')))) {
        return Err("terminal launch path has an ambiguous DOS spelling".into());
    }
    let wide: Vec<u16> = canonical.as_os_str().encode_wide().collect();
    let path = match canonical.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::VerbatimDisk(_) => PathBuf::from(std::ffi::OsString::from_wide(&wide[4..])),
            Prefix::VerbatimUNC(_, _) => {
                let mut unc = vec![u16::from(b'\\'), u16::from(b'\\')];
                unc.extend_from_slice(&wide[8..]);
                PathBuf::from(std::ffi::OsString::from_wide(&unc))
            }
            Prefix::Disk(_) | Prefix::UNC(_, _) => canonical.to_path_buf(),
            _ => return Err("terminal launch requires a drive-letter or UNC path".into()),
        },
        _ => return Err("terminal launch path must be an absolute Windows path".into()),
    };
    Ok(path)
}

pub(in crate::dogfood) fn shell_cwd(canonical: &Path, cmd_shell: bool) -> Result<PathBuf, String> {
    if cmd_shell
        && !matches!(
            canonical.components().next(),
            Some(Component::Prefix(prefix))
                if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
        )
    {
        return Err(
            "cmd.exe requires a drive-letter workdir; select PowerShell for UNC paths".into(),
        );
    }
    launch_path(canonical)
}

#[cfg(test)]
mod tests {
    use super::{launch_path, ordinary_path, shell_cwd};
    use std::path::Path;

    #[test]
    fn verbatim_drive_and_unc_spellings_keep_their_windows_roots() -> Result<(), String> {
        for (given, expected) in [
            (
                r"\\?\C:\Windows\powershell.exe",
                r"C:\Windows\powershell.exe",
            ),
            (
                r"\\?\UNC\server\share\shell.exe",
                r"\\server\share\shell.exe",
            ),
        ] {
            assert_eq!(ordinary_path(Path::new(given))?, Path::new(expected));
        }
        assert!(ordinary_path(Path::new(r"\\.\device")).is_err());
        assert!(ordinary_path(Path::new(r"C:relative")).is_err());
        assert!(ordinary_path(Path::new(r"\\?\C:\trailing.\shell.exe")).is_err());
        assert!(ordinary_path(Path::new(r"\\?\C:\trailing \shell.exe")).is_err());
        Ok(())
    }

    #[test]
    fn the_native_powershell_program_round_trips_without_its_verbatim_prefix() -> Result<(), String>
    {
        let root = std::env::var_os("SystemRoot").ok_or("SystemRoot missing")?;
        let canonical = std::fs::canonicalize(
            std::path::PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe"),
        )
        .map_err(|error| error.to_string())?;
        let launched = launch_path(&canonical)?;
        assert!(!launched.to_string_lossy().starts_with(r"\\?\"));
        assert_eq!(
            std::fs::canonicalize(&launched).map_err(|error| error.to_string())?,
            canonical
        );
        Ok(())
    }

    #[test]
    fn cmd_refuses_unc_and_drive_relative_workdirs_before_accessing_them() {
        for cwd in [
            r"\\server\share\work",
            r"\\?\UNC\server\share\work",
            r"C:relative",
        ] {
            assert!(shell_cwd(Path::new(cwd), true).is_err(), "{cwd}");
        }
    }
}
