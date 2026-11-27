//! The ConPTY root that makes a terminal's job-wide exit observable.
//!
//! conpty-oxide keeps its session job private and resumes the child inside
//! its own spawn, so nothing outside it can confirm that a terminal's
//! descendants have exited. This executable is started as the pseudoconsole
//! root in shim mode instead. The shim starts the real shell on the inherited
//! pseudoconsole through [`ConfirmedJob`], waits for it, terminates and
//! confirms the job within its bound, reports once and exits with the shell's
//! status unchanged. Console input, including Ctrl-C, reaches the shell
//! through the shared console exactly as before.
//!
//! The product owns two named pipes per terminal, one per direction: a
//! synchronous pipe handle serializes reads against writes, so one shared
//! handle would let a blocked control read hold back the final report.
use super::shim_protocol::{self, Cleanup, ShellLaunch, ShimInvocation, ShimReport};
use super::windows_job::ConfirmedJob;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

const REPORT_SUFFIX: &str = "-report";
const CONTROL_SUFFIX: &str = "-control";

/// Run shim mode when `args` select it; `None` otherwise.
pub(crate) fn shim_main(args: &[OsString]) -> Option<i32> {
    if let Some(code) = super::super::sandbox::windows::helper_main(args) {
        return Some(code);
    }
    Some(match shim_protocol::decode(args)? {
        Ok(invocation) => run(invocation),
        Err(reason) => {
            eprintln!("{reason}");
            1
        }
    })
}

fn run(invocation: ShimInvocation) -> i32 {
    let pipe = |suffix: &str| {
        let mut name = invocation.pipe.clone();
        name.push(suffix);
        name
    };
    // Nothing runs unless the owner can hear the outcome.
    let opened = std::fs::OpenOptions::new()
        .write(true)
        .open(pipe(REPORT_SUFFIX))
        .and_then(|report| {
            std::fs::OpenOptions::new()
                .read(true)
                .open(pipe(CONTROL_SUFFIX))
                .map(|control| (report, control))
        });
    let (mut report, control) = match opened {
        Ok(pipes) => pipes,
        Err(error) => {
            eprintln!("terminal shim could not reach its owner: {error}");
            return 1;
        }
    };
    let (exit, cleanup, diagnostic) = supervise(&invocation.launch, control);
    let _sent = report
        .write_all(&shim_protocol::encode_report(&ShimReport {
            exit,
            cleanup,
            diagnostic,
        }))
        .and_then(|()| report.flush());
    // Windows uses all 32 bits for exit status. Preserve its bit pattern.
    i32::from_ne_bytes(exit.to_ne_bytes())
}

#[cfg_attr(windows, expect(clippy::too_many_lines, reason = "legacy"))]
fn supervise(
    launch: &ShellLaunch,
    mut control: std::fs::File,
) -> (u32, Cleanup, Option<serde_json::Value>) {
    let failed = |reason: String| (1, Cleanup::Unconfirmed(reason), None);
    // Held until this process exits, before the shell can receive anything.
    let _interrupts = match hold_interrupts() {
        Ok(held) => held,
        Err(reason) => return failed(reason),
    };
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => return failed(format!("terminal shim workdir is unavailable: {error}")),
    };
    let program = match launch {
        ShellLaunch::Cmd { program, .. } | ShellLaunch::Exec { program, .. } => program,
    };
    let context = super::windows_diagnostics::Context::new(
        std::path::Path::new(program),
        None,
        &cwd,
        None,
        "conpty-attached-shim",
    );
    let mut job = match ConfirmedJob::create_observed(&context) {
        Ok(job) => job,
        Err(failure) => return (1, Cleanup::Unconfirmed(failure.detail), failure.diagnostic),
    };
    let mut command = match launch {
        ShellLaunch::Cmd { program, command } => {
            let mut built = windows_spawn::Command::new(program);
            let mut quoted = OsString::from("\"");
            quoted.push(command);
            quoted.push("\"");
            built.args(["/D", "/S", "/C"]).raw_arg(quoted);
            built
        }
        ShellLaunch::Exec { program, arguments } => {
            let mut built = windows_spawn::Command::new(program);
            match super::super::sandbox::windows::forwarded_command(program, arguments) {
                Ok(Some(command)) => {
                    job.observe_msys();
                    command
                }
                Ok(None) => {
                    built.args(arguments);
                    built
                }
                Err(reason) => return failed(reason),
            }
        }
    };
    let job = Arc::new(job);
    // The shell attaches to this process's console, the pseudoconsole, but
    // its standard streams must name that console too. A pseudoconsole root is
    // started with null standard handles, so inheriting them would leave the
    // shell writing nowhere and reading end-of-input. Open the attached
    // console itself instead.
    let capture = windows_spawn::diagnostics::Capture::new();
    let streams = match console_streams() {
        Ok(streams) => streams,
        Err(reason) => {
            return (
                1,
                Cleanup::Unconfirmed(reason),
                Some(context.failure(
                    capture.failure(),
                    None,
                    serde_json::json!({"phase":"console_streams","policy":"kill_on_close"}),
                )),
            )
        }
    };
    drop(capture);
    command.stdin(streams.0).stdout(streams.1).stderr(streams.2);
    let mut child = match job.spawn_tracked(
        &mut command,
        windows_spawn::CreationFlags::default(),
        &context,
    ) {
        Ok(child) => child,
        Err(failure) => {
            // Creation can fail after the process existed; confirm regardless.
            let reason = match job.terminate_and_confirm(1) {
                Ok(()) => failure.detail,
                Err(cleanup) => format!("{}; {cleanup}", failure.detail),
            };
            return (1, Cleanup::Unconfirmed(reason), failure.diagnostic);
        }
    };
    drop(context);
    let watched = Arc::clone(&job);
    // The owner asks for termination with one byte, and a closed control pipe
    // means the owner is gone: either way the shell's job stops. An interrupt
    // the console could not deliver ends the job with Ctrl-C's own status.
    let _control = std::thread::Builder::new()
        .name("grapt-terminal-shim-control".into())
        .spawn(move || {
            let mut byte = [0_u8; 1];
            let status = loop {
                match control.read(&mut byte) {
                    Ok(1) if byte[0] == shim_protocol::TERMINATE => break 1,
                    Ok(1) if byte[0] == shim_protocol::INTERRUPT => {
                        break shim_protocol::CONTROL_C_EXIT
                    }
                    Ok(1) => continue,
                    _ => break 1,
                }
            };
            let _stopped = watched.terminate(status);
        });
    let exit = match child.wait() {
        Ok(status) => match status.code() {
            Some(code) => u32::from_ne_bytes(code.to_ne_bytes()),
            None => return confirm_after(&job, "terminal shell exit has no code".into()),
        },
        Err(error) => {
            return confirm_after(
                &job,
                format!("terminal shell exit observation failed: {error}"),
            );
        }
    };
    job.stop_msys();
    if job.msys_observation_requested() {
        if let Some(note) = super::super::sandbox::msys::failure_note(
            i32::from_ne_bytes(exit.to_ne_bytes()),
            job.observed_msys(),
        ) {
            eprintln!("\n[{note}]");
        }
    }
    let cleanup = match job.terminate_and_confirm(1) {
        Ok(()) => Cleanup::Confirmed,
        Err(reason) => Cleanup::Unconfirmed(reason),
    };
    (exit, cleanup, None)
}

/// Console interrupts reach every process on the console, this shim included.
/// The default handler would end the shim before it could confirm and report,
/// so the shim takes Ctrl-C and Ctrl-Break itself: a live listener makes the
/// handler report them handled. The shell's own handling is unchanged; it
/// still receives each interrupt through the shared console.
pub(in crate::dogfood::tools) struct Interrupts {
    _ctrl_c: tokio::signal::windows::CtrlC,
    _ctrl_break: tokio::signal::windows::CtrlBreak,
    _runtime: tokio::runtime::Runtime,
}

pub(in crate::dogfood::tools) fn hold_interrupts() -> Result<Interrupts, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("terminal shim runtime unavailable: {error}"))?;
    let entered = runtime.enter();
    let unavailable =
        |error: std::io::Error| format!("terminal shim interrupt handler unavailable: {error}");
    let ctrl_c = tokio::signal::windows::ctrl_c().map_err(unavailable)?;
    let ctrl_break = tokio::signal::windows::ctrl_break().map_err(unavailable)?;
    drop(entered);
    Ok(Interrupts {
        _ctrl_c: ctrl_c,
        _ctrl_break: ctrl_break,
        _runtime: runtime,
    })
}

/// Standard input, output and error on this process's console.
fn console_streams() -> Result<
    (
        windows_spawn::Stdio,
        windows_spawn::Stdio,
        windows_spawn::Stdio,
    ),
    String,
> {
    let open = |name: &str| {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(name)
            .map_err(|error| {
                let call = if name == "CONIN$" {
                    "CreateFileW(CONIN$)"
                } else {
                    "CreateFileW(CONOUT$)"
                };
                windows_spawn::diagnostics::record(call, &error);
                format!("terminal shim has no console {name}: {error}")
            })
    };
    let input = open("CONIN$")?;
    let output = open("CONOUT$")?;
    let stream = |file: &std::fs::File| {
        windows_spawn::Stdio::from_borrowed(file)
            .map_err(|error| format!("terminal shim console handle unavailable: {error}"))
    };
    Ok((stream(&input)?, stream(&output)?, stream(&output)?))
}

fn confirm_after(job: &ConfirmedJob, reason: String) -> (u32, Cleanup, Option<serde_json::Value>) {
    let detail = match job.terminate_and_confirm(1) {
        Ok(()) => reason,
        Err(cleanup) => format!("{reason}; {cleanup}"),
    };
    (1, Cleanup::Unconfirmed(detail), None)
}

/// What the product has heard from one terminal's shim.
#[derive(Debug, Clone)]
pub(super) enum Heard {
    Nothing,
    Report(ShimReport),
    Closed(String),
}

/// The product's end of one shim's pipes.
pub(super) struct Link {
    heard: Arc<Mutex<Heard>>,
    control: Option<tokio::sync::mpsc::Sender<u8>>,
    tasks: [tokio::task::AbortHandle; 2],
}

static NEXT_LINK: AtomicU64 = AtomicU64::new(0);

impl Link {
    /// Create both pipes before the shim exists. `first_pipe_instance`
    /// refuses a name another process already holds.
    pub fn listen(
        observed_ms: Option<u64>,
        concurrent: usize,
        context: &super::windows_diagnostics::Context,
    ) -> Result<(OsString, Self), super::process::SpawnFailure> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|error| format!("terminal shim link needs the runtime: {error}"))?;
        let _entered = runtime.enter();
        let base = format!(
            r"\\.\pipe\grapt-terminal-{}-{}",
            std::process::id(),
            NEXT_LINK.fetch_add(1, Ordering::Relaxed)
        );
        let server = |suffix: &str, inbound: bool| {
            ServerOptions::new()
                .first_pipe_instance(true)
                .reject_remote_clients(true)
                .max_instances(1)
                .access_inbound(inbound)
                .access_outbound(!inbound)
                .create(format!("{base}{suffix}"))
                .map_err(|error| {
                    let mut failure = super::process::SpawnFailure::from(format!("terminal shim pipe unavailable: {error}"));
                    failure.diagnostic = Some(context.io_failure("CreateNamedPipeW(shim)", &error,
                        serde_json::json!({"phase":"before_console","channel":suffix,"console_state":"not_created"})));
                    failure
                })
        };
        let report = server(REPORT_SUFFIX, true)?;
        let control = server(CONTROL_SUFFIX, false)?;
        let heard = Arc::new(Mutex::new(Heard::Nothing));
        let (control_bytes, mut requested) = tokio::sync::mpsc::channel::<u8>(2);
        let listened = Arc::clone(&heard);
        let reading = runtime.spawn(async move {
            let outcome = read_report(report).await;
            *super::lock(&listened) = match outcome {
                Ok(mut report) => {
                    if let Some(record) = report.diagnostic.take() {
                        super::windows_diagnostics::received(record, observed_ms, concurrent);
                    }
                    Heard::Report(report)
                }
                Err(reason) => Heard::Closed(reason),
            };
        });
        let controlling = runtime.spawn(async move {
            if control.connect().await.is_ok() {
                let mut control = control;
                while let Some(byte) = requested.recv().await {
                    let _sent = control.write_all(&[byte]).await;
                    let _flushed = control.flush().await;
                }
                // Hold the pipe open: closing it would also read as a stop.
                std::future::pending::<()>().await;
            }
        });
        Ok((
            OsString::from(base),
            Self {
                heard,
                control: Some(control_bytes),
                tasks: [reading.abort_handle(), controlling.abort_handle()],
            },
        ))
    }

    pub fn heard(&self) -> Heard {
        super::lock(&self.heard).clone()
    }

    /// Ask the shim to stop the shell's job. False once a stop was asked.
    pub fn request_terminate(&mut self) -> bool {
        self.control
            .take()
            .is_some_and(|control| control.try_send(shim_protocol::TERMINATE).is_ok())
    }

    /// Ask the shim to end the job with the Ctrl-C status. False once a stop
    /// was asked; a later stop request still goes through.
    pub fn request_interrupt(&mut self) -> bool {
        self.control
            .as_ref()
            .is_some_and(|control| control.try_send(shim_protocol::INTERRUPT).is_ok())
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn read_report(report: NamedPipeServer) -> Result<ShimReport, String> {
    report
        .connect()
        .await
        .map_err(|error| format!("terminal shim never connected: {error}"))?;
    let mut line = Vec::new();
    tokio::io::BufReader::new(report)
        .take(shim_protocol::MAX_REPORT_BYTES as u64)
        .read_until(b'\n', &mut line)
        .await
        .map_err(|error| format!("terminal shim report was not received: {error}"))?;
    if line.is_empty() {
        return Err("terminal shim exited without reporting job-wide exit".into());
    }
    shim_protocol::parse_report(&line)
}

/// The shim's arguments for one terminal.
pub(super) fn arguments(pipe: &OsStr, launch: &ShellLaunch) -> Vec<OsString> {
    shim_protocol::encode(pipe, launch)
}
