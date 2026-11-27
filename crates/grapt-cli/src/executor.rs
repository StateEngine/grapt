//! Public command executor and process orchestration seams.

use std::future::Future;
use std::io::{self, Write};
use std::time::Duration;

use tokio::io::AsyncReadExt;

use crate::command::{ConnectionChoice, PromptSource, ServiceAction};
use crate::{
    ApiClient, BoundedOutput, CancellationCause, ChildLiveness, CliCommand, CliLimits,
    ClientCredential, ClientEndpoint, DiscoveryError, ExitClass, LaunchWindowObservation,
    OutputError, OutputMode, ReadinessWait, RunError, RunReceipt, Runner, ServiceDiscovery,
    SessionSelector, CANCELLATION_HANDOFF,
};

mod acp;
mod connection_client;
mod connections;
mod ephemeral;
mod images;
pub(crate) mod import;
mod import_source;
mod login;
mod lossy_stderr;
mod mcp;
mod plugin;
mod session;
mod signals;

use signals::termination_signal;

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
pub async fn execute(command: CliCommand) -> u8 {
    let limits = CliLimits::production();
    match command {
        CliCommand::Mcp(command) => mcp::execute(command, limits).await,
        CliCommand::Plugin(command) => plugin::execute(command, limits).await,
        CliCommand::Import(command) => import::execute(command, limits).await,
        CliCommand::FocusedHelp(topic) => {
            let mut sink = output_sink(OutputMode::Text, limits);
            match sink.text(topic.text()) {
                Ok(()) => ExitClass::Success.code(),
                Err(error) => output_failure(&mut sink, error),
            }
        }
        CliCommand::Help => {
            let mut sink = output_sink(OutputMode::Text, limits);
            match sink.text(USAGE) {
                Ok(()) => ExitClass::Success.code(),
                Err(error) => output_failure(&mut sink, error),
            }
        }
        CliCommand::Version => {
            let mut sink = output_sink(OutputMode::Text, limits);
            // The crate version alone cannot tell two builds apart; the second
            // line is the attested build, or says why there is none.
            let version = format!(
                "grapt {}\n{}",
                env!("CARGO_PKG_VERSION"),
                crate::build_identity::of_running_binary()
            );
            match sink.text(&version) {
                Ok(()) => ExitClass::Success.code(),
                Err(error) => output_failure(&mut sink, error),
            }
        }
        CliCommand::SessionProvision(command) => crate::seat::execute(command).await,
        CliCommand::Connections(command) => connections::execute(command, limits).await,
        CliCommand::SessionRename {
            connection,
            session,
            title,
        } => session::rename(connection, session, title, limits).await,
        CliCommand::Service {
            action,
            service_root,
            product_binary,
        } => service(action, service_root, product_binary, limits).await,
        CliCommand::ServiceStatus {
            service_root,
            output,
        } => {
            let mut sink = output_sink(output, limits);
            match ServiceDiscovery::try_new(service_root, env!("CARGO_PKG_VERSION")) {
                Ok(discovery) => match discovery.discover().await {
                    Ok(service) => match emit_service(&mut sink, &service, "ready") {
                        Ok(()) => ExitClass::Success.code(),
                        Err(error) => output_failure(&mut sink, error),
                    },
                    Err(error) => diagnostic_exit(&mut sink, ExitClass::Discovery, &error),
                },
                Err(error) => diagnostic_exit(&mut sink, ExitClass::Discovery, &error),
            }
        }
        CliCommand::Launch {
            service_root,
            product_binary,
            output,
        } => launch(service_root, product_binary, output, limits).await,
        CliCommand::Attach {
            connection,
            selector,
            output,
        } => {
            let mut sink = output_sink(output, limits);
            match connect(connection, limits).await {
                Ok(client) => match Runner::new(client).attach(selector, &mut sink).await {
                    Ok(receipt) => receipt_exit(&mut sink, &receipt),
                    Err(error) => diagnostic_exit(&mut sink, error.exit_class(), &error),
                },
                Err((class, message)) => diagnostic_exit(&mut sink, class, &message),
            }
        }
        CliCommand::Run {
            connection,
            selector,
            prompt,
            input_id,
            images,
            title,
            output,
            reasoning,
            reasoning_effort,
            selection,
            max_steps,
            fork,
            ephemeral,
            background,
            resident,
            worktree,
            preset,
            auto,
        } => {
            let mut sink = output_sink(output, limits);
            if ephemeral
                && (!matches!(&connection, ConnectionChoice::Embedded { .. })
                    || selector != SessionSelector::New
                    || fork
                    || background
                    || resident
                    || worktree.is_some())
            {
                return diagnostic_exit(
                    &mut sink,
                    ExitClass::Usage,
                    &crate::CommandError::EphemeralTarget,
                );
            }
            let prompt = match prompt {
                PromptSource::Argument(prompt) => prompt,
                PromptSource::Stdin => match read_stdin(limits).await {
                    Ok(prompt) => prompt,
                    Err(message) => return diagnostic_exit(&mut sink, ExitClass::Usage, &message),
                },
            };
            let images = match images::load(&images) {
                Ok(images) => images,
                Err(error) => return diagnostic_exit(&mut sink, ExitClass::Usage, &error),
            };
            let connected = if ephemeral {
                ephemeral::connect_ephemeral(connection, limits).await
            } else {
                connect(connection, limits).await
            };
            match connected {
                Ok(client) => {
                    let ephemeral_owner = ephemeral.then(|| client.clone());
                    let runner = Runner::new(client)
                        .with_input_id(input_id)
                        .with_images(images)
                        .with_title(title)
                        .with_reasoning(reasoning)
                        .with_reasoning_effort(reasoning_effort)
                        .with_selection(selection)
                        .with_max_steps(max_steps)
                        .with_fork(fork)
                        .with_worktree(worktree)
                        .with_preset(preset)
                        .with_auto(auto)
                        .with_resident(resident);
                    if background {
                        return match runner.start_background(selector, prompt, &mut sink).await {
                            Ok(_) => ExitClass::Success.code(),
                            Err(error) => diagnostic_exit(&mut sink, error.exit_class(), &error),
                        };
                    }
                    let (result, mut sink) =
                        run_with_cancellation(runner, selector, prompt, sink, termination_signal())
                            .await;
                    if let Some(owner) = ephemeral_owner {
                        if let Err(cleanup) = ephemeral::close(&owner).await {
                            let message = match &result {
                                Ok(_) => cleanup,
                                Err(error) => format!("{error}; {cleanup}"),
                            };
                            return match sink.as_mut() {
                                Some(sink) => diagnostic_exit(sink, ExitClass::Child, &message),
                                None => ExitClass::Child.code(),
                            };
                        }
                    }
                    match (result, sink) {
                        (Ok(receipt), Some(mut sink)) => receipt_exit(&mut sink, &receipt),
                        (Err(error), Some(mut sink)) => {
                            diagnostic_exit(&mut sink, error.exit_class(), &error)
                        }
                        // The output stayed with a write its reader is not
                        // draining; the exit code alone reports the outcome.
                        (Ok(receipt), None) => receipt.exit.code(),
                        (Err(error), None) => error.exit_class().code(),
                    }
                }
                Err((class, message)) => diagnostic_exit(&mut sink, class, &message),
            }
        }
        CliCommand::Acp { connection } => acp::execute(connection, limits).await,
    }
}

/// Runs a prompt while keeping the termination signal reachable.
///
/// Output is written synchronously, so a stdout reader that stops draining
/// blocks the task doing the write. The run therefore owns the output on its
/// own task, and the signal is awaited here. On a signal the run is stopped and
/// hands the output back; if it cannot within [`CANCELLATION_HANDOFF`], it is
/// blocked in a write, and the Session is interrupted without it. The output is
/// returned only when it came back.
pub async fn run_with_cancellation<O, E, F>(
    runner: Runner,
    selector: SessionSelector,
    prompt: String,
    mut output: BoundedOutput<O, E>,
    signal: F,
) -> (Result<RunReceipt, RunError>, Option<BoundedOutput<O, E>>)
where
    O: Write + Send + 'static,
    E: Write + Send + 'static,
    F: Future<Output = io::Result<CancellationCause>>,
{
    let (session, attempts) = match runner.select_session(selector, output.receipt()).await {
        Ok(selected) => selected,
        Err(error) => return (Err(error), Some(output)),
    };
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let mut run = tokio::spawn({
        let runner = runner.for_task();
        let session = session.clone();
        async move {
            let result = tokio::select! {
                result = runner.run_selected(session, attempts, prompt, &mut output) => Some(result),
                _stop = stopped => None,
            };
            (result, output)
        }
    });
    tokio::select! {
        joined = &mut run => match joined {
            Ok((Some(result), output)) => (result, Some(output)),
            // Only the stop sent on a signal below ends a run early.
            Ok((None, output)) => (Err(RunError::CancellationRace), Some(output)),
            Err(error) => std::panic::resume_unwind(error.into_panic()),
        },
        signal = signal => {
            let _stop_result = stop.send(());
            let output = match tokio::time::timeout(CANCELLATION_HANDOFF, run).await {
                Ok(Ok((_stopped, output))) => Some(output),
                Ok(Err(error)) => std::panic::resume_unwind(error.into_panic()),
                Err(_blocked_in_write) => None,
            };
            let result = match signal {
                Ok(cause) => runner.cancel(session, cause).await,
                Err(error) => Err(RunError::Signal(error.kind())),
            };
            (result, output)
        }
    }
}

pub async fn connect(
    connection: ConnectionChoice,
    limits: CliLimits,
) -> Result<ApiClient, (ExitClass, String)> {
    match connection {
        ConnectionChoice::Discover { service_root } => {
            let discovery = ServiceDiscovery::try_new(service_root, env!("CARGO_PKG_VERSION"))
                .map_err(|error| (ExitClass::Discovery, error.to_string()))?;
            let service = discovery
                .discover()
                .await
                .map_err(|error| (ExitClass::Discovery, error.to_string()))?;
            ApiClient::from_discovered(&service, limits)
                .map_err(|error| (ExitClass::Protocol, error.to_string()))
        }
        ConnectionChoice::Remote {
            endpoint,
            credential_file,
        } => {
            let endpoint = ClientEndpoint::remote(&endpoint)
                .map_err(|error| (ExitClass::Protocol, error.to_string()))?;
            let credential = ClientCredential::read(&credential_file)
                .map_err(|error| (ExitClass::Discovery, error.to_string()))?;
            ApiClient::remote(endpoint, &credential, limits)
                .map_err(|error| (ExitClass::Protocol, error.to_string()))
        }
        ConnectionChoice::Embedded {
            product_binary,
            service_root,
        } => embedded_client(product_binary, service_root, limits).await,
    }
}

pub async fn embedded_client(
    product_binary: std::path::PathBuf,
    service_root: std::path::PathBuf,
    limits: CliLimits,
) -> Result<ApiClient, (ExitClass, String)> {
    crate::client::chatgpt::renew_before_start(&service_root)
        .await
        .map_err(|error| (ExitClass::Discovery, error))?;
    embedded_client_inner(product_binary, service_root.clone(), limits)
        .await
        .map_err(|(class, message)| (class, crate::server_log::failure(&service_root, message)))
}

async fn embedded_client_inner(
    product_binary: std::path::PathBuf,
    service_root: std::path::PathBuf,
    limits: CliLimits,
) -> Result<ApiClient, (ExitClass, String)> {
    embedded_client_mode(product_binary, service_root, limits, "serve-terminal", true).await
}

async fn embedded_client_mode(
    product_binary: std::path::PathBuf,
    service_root: std::path::PathBuf,
    limits: CliLimits,
    verb: &'static str,
    log: bool,
) -> Result<ApiClient, (ExitClass, String)> {
    let stderr = if log {
        std::process::Stdio::from(crate::server_log::open(&service_root).map_err(|error| {
            (
                ExitClass::Child,
                format!("server log cannot be opened: {error}"),
            )
        })?)
    } else {
        std::process::Stdio::inherit()
    };
    let mut launch = tokio::process::Command::new(&product_binary);
    grapt_service::launcher_environment::scrub(launch.as_std_mut());
    let mut child = launch
        .arg(verb)
        .arg("--service")
        .arg(&service_root)
        // The product serves only `--port 0`: it binds and publishes its own port.
        .arg("--port")
        .arg("0")
        // The existing terminal lifetime observes EOF when this parent dies.
        // Retain the write end in the owned child until shutdown starts.
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(stderr)
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| (ExitClass::Child, format!("child launch failed: {error}")))?;
    let child_pid = child.id().ok_or_else(|| {
        (
            ExitClass::Child,
            "embedded child has no process identity".to_owned(),
        )
    })?;
    let discovery = ServiceDiscovery::try_new(service_root, env!("CARGO_PKG_VERSION"))
        .map_err(|error| (ExitClass::Discovery, error.to_string()))?;
    let child_alive = || match child.try_wait() {
        Ok(Some(status)) => {
            ChildLiveness::Exited(format!("embedded child exited before readiness: {status}"))
        }
        Ok(None) => ChildLiveness::Alive,
        Err(error) => ChildLiveness::StatusFailed(format!("embedded child status failed: {error}")),
    };
    let ready = discovery
        .await_child_readiness(child_pid, child_alive)
        .await;
    match ready {
        Ok(service) => match ApiClient::from_embedded(&service, child, limits) {
            Ok(client) => Ok(client),
            // A refused client hands the child back for the same cleanup.
            Err((error, child)) => Err((
                ExitClass::Protocol,
                reap_embedded_failure(error.to_string(), child).await,
            )),
        },
        // The status read that saw the exit already reaped the child.
        Err(ReadinessWait::Child(message)) => Err((ExitClass::Child, message)),
        // Unknown child state: kill and reap before refusing.
        Err(ReadinessWait::StatusFailed(message)) => Err((
            ExitClass::Child,
            reap_embedded_failure(message, child).await,
        )),
        Err(ReadinessWait::Discovery(error)) => Err((
            ExitClass::Discovery,
            reap_embedded_failure(error.to_string(), child).await,
        )),
        Err(ReadinessWait::Window(observation)) => Err((
            ExitClass::Child,
            reap_embedded_failure(
                window_refusal(EMBEDDED_WINDOW, child_pid, &observation),
                child,
            )
            .await,
        )),
    }
}

/// The refusal when an embedded child exhausts the launch window.
const EMBEDDED_WINDOW: &str =
    "embedded child did not publish its own attributed readiness within the finite launch window";

/// The refusal when a started service owner exhausts the launch window.
const SERVICE_WINDOW: &str =
    "child did not publish its own attributed readiness within the finite launch window";

/// The window refusal: the still-alive child stated first, then what the
/// window ended on - the last pending probe's refusal, or that no probe
/// completed. The exit class stays [`ExitClass::Child`]: the child was alive
/// at the deadline and is reaped here, distinct from a service that never
/// stood up at all (`ExitClass::Discovery`).
pub(crate) fn window_refusal(
    core_text: &str,
    child_pid: u32,
    observation: &LaunchWindowObservation,
) -> String {
    let waited = crate::discovery::LAUNCH_WINDOW.as_secs();
    let alive = format!(
        "the service is still starting (slow disk?): waited {waited}s; process {child_pid} alive"
    );
    match observation.last_pending() {
        Some(last) => format!(
            "{alive}; {core_text} ({} polls; last probe: {last})",
            observation.polls()
        ),
        None => format!("{alive}; {core_text} (no probe saw the child publishing)"),
    }
}

/// How long a failed launch waits for the kernel to reap its killed child.
/// The kill is send-only (`start_kill` / `TerminateProcess`), so this bound is
/// the only wait.
const CLEANUP_REAP_BOUND: Duration = Duration::from_secs(10);

fn kill_note(kill_error: Option<&std::io::Error>) -> String {
    match kill_error {
        Some(error) => format!("; the kill itself was refused: {error}"),
        None => String::new(),
    }
}

/// Names a child the bounded cleanup could not reap: pid, what the bound
/// observed, and the kill's own refusal if any.
fn residue_message(pid: Option<u32>, detail: &str, kill_error: Option<&std::io::Error>) -> String {
    let child = match pid {
        Some(pid) => format!("the exact child (pid {pid})"),
        None => "the exact child".to_owned(),
    };
    format!("{child} - {detail}{}", kill_note(kill_error))
}

/// `windows-spawn` has no async wait, so the listed child's reap polls
/// `try_wait` at this pace until [`bound_reap`]'s deadline.
#[cfg(windows)]
const REAP_POLL: Duration = Duration::from_millis(50);

/// Wait for an already-killed child's reap, cut at [`CLEANUP_REAP_BOUND`].
/// The wait is driven by the runtime (pidfd/SIGCHLD on unix), so a timed-out
/// wait leaves no thread behind for runtime shutdown to join.
async fn bound_reap(
    kill_error: Option<io::Error>,
    wait: impl Future<Output = io::Result<std::process::ExitStatus>>,
) -> Result<std::process::ExitStatus, (String, Option<io::Error>)> {
    let bound_text = || {
        format!(
            "it was not reaped within its {}-second cleanup bound",
            CLEANUP_REAP_BOUND.as_secs()
        )
    };
    match tokio::time::timeout(CLEANUP_REAP_BOUND, wait).await {
        Ok(Ok(status)) => Ok(status),
        Ok(Err(error)) => Err((format!("it was not reaped: {error}"), kill_error)),
        Err(_) => Err((bound_text(), kill_error)),
    }
}

/// The listed child's reap wait: `try_wait` every [`REAP_POLL`].
#[cfg(windows)]
async fn listed_child_wait(
    child: &mut windows_spawn::Child,
) -> io::Result<std::process::ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => tokio::time::sleep(REAP_POLL).await,
            Err(error) => return Err(error),
        }
    }
}

/// Kill and reap a failed service launch; a cleanup failure is appended to
/// the refusal, never replacing it.
async fn reap_failed_launch(core: String, child: ServiceOwner) -> String {
    match child.reap().await {
        Ok(_) => core,
        Err(residue) => {
            format!("{core}; the spawned child's bounded cleanup failed: {residue}")
        }
    }
}

/// The same kill-and-bounded-reap for the embedded tokio child, which
/// `kill_on_drop` only backstops.
async fn reap_embedded_failure(core: String, mut child: tokio::process::Child) -> String {
    let pid = child.id();
    let kill_error = child.start_kill().err();
    match bound_reap(kill_error, child.wait()).await {
        Ok(_) => core,
        Err((detail, kill_error)) => format!(
            "{core}; the spawned child's bounded cleanup failed: {}",
            residue_message(pid, &detail, kill_error.as_ref())
        ),
    }
}

pub async fn launch(
    service_root: std::path::PathBuf,
    product_binary: std::path::PathBuf,
    output: OutputMode,
    limits: CliLimits,
) -> u8 {
    let mut sink = output_sink(output, limits);
    match start_service(service_root, product_binary).await {
        Ok(service) => match emit_service(&mut sink, &service, "launched") {
            Ok(()) => ExitClass::Success.code(),
            Err(error) => output_failure(&mut sink, error),
        },
        Err((class, message)) => diagnostic_exit(&mut sink, class, &message),
    }
}

/// Starts the product as the service owner of `service_root` and waits for its
/// own attributed readiness.
async fn start_service(
    service_root: std::path::PathBuf,
    product_binary: std::path::PathBuf,
) -> Result<crate::DiscoveredService, (ExitClass, String)> {
    start_service_inner(service_root.clone(), product_binary)
        .await
        .map_err(|(class, message)| (class, crate::server_log::failure(&service_root, message)))
}

async fn start_service_inner(
    service_root: std::path::PathBuf,
    product_binary: std::path::PathBuf,
) -> Result<crate::DiscoveredService, (ExitClass, String)> {
    crate::client::chatgpt::renew_before_start(&service_root)
        .await
        .map_err(|error| (ExitClass::Discovery, error))?;
    let mut child = spawn_service_owner(&product_binary, &service_root)
        .map_err(|error| (ExitClass::Child, error.to_string()))?;
    let child_pid = child
        .id()
        .ok_or_else(|| (ExitClass::Child, "child has no process identity".to_owned()))?;
    let discovery = ServiceDiscovery::try_new(service_root, env!("CARGO_PKG_VERSION"))
        .map_err(|error| (ExitClass::Discovery, error.to_string()))?;
    let child_alive = || match child.try_wait() {
        Ok(Some(status)) => ChildLiveness::Exited(status.to_string()),
        Ok(None) => ChildLiveness::Alive,
        Err(error) => ChildLiveness::StatusFailed(error.to_string()),
    };
    match discovery
        .await_child_readiness(child_pid, child_alive)
        .await
    {
        Ok(service) => Ok(service),
        // The status read that saw the exit already reaped the child.
        Err(ReadinessWait::Child(message)) => Err((ExitClass::Child, message)),
        // Unknown child state: kill and reap before refusing.
        Err(ReadinessWait::StatusFailed(message)) => {
            Err((ExitClass::Child, reap_failed_launch(message, child).await))
        }
        Err(ReadinessWait::Discovery(error)) => Err((
            ExitClass::Discovery,
            reap_failed_launch(error.to_string(), child).await,
        )),
        Err(ReadinessWait::Window(observation)) => Err((
            ExitClass::Child,
            reap_failed_launch(
                window_refusal(SERVICE_WINDOW, child_pid, &observation),
                child,
            )
            .await,
        )),
    }
}

/// The product `start_service` launches as the service owner. It outlives the
/// CLI, so it must not hold anything the CLI's caller waits on. A failed
/// launch kills and reaps it before refusing.
enum ServiceOwner {
    /// Spawned with an explicit handle list: a std spawn on Windows hands the
    /// child every inheritable handle of the CLI, including a pipe its caller
    /// is reading, and the caller would wait until the service exits.
    #[cfg(windows)]
    Listed(windows_spawn::Child),
    #[cfg(not(windows))]
    Std(tokio::process::Child),
}

impl ServiceOwner {
    fn id(&self) -> Option<u32> {
        match self {
            #[cfg(windows)]
            Self::Listed(child) => Some(child.id()),
            #[cfg(not(windows))]
            Self::Std(child) => child.id(),
        }
    }

    fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        match self {
            #[cfg(windows)]
            Self::Listed(child) => child.try_wait(),
            #[cfg(not(windows))]
            Self::Std(child) => child.try_wait(),
        }
    }

    /// Kill and reap this owner's exact child within [`CLEANUP_REAP_BOUND`],
    /// by its own handle only (no pid scan or process group). A child that
    /// outlives the bound is reported by name.
    async fn reap(self) -> Result<std::process::ExitStatus, String> {
        let pid = self.id();
        match self {
            #[cfg(windows)]
            Self::Listed(mut child) => {
                let kill_error = child.kill().err();
                match bound_reap(kill_error, listed_child_wait(&mut child)).await {
                    Ok(status) => Ok(status),
                    Err((detail, kill_error)) => {
                        Err(residue_message(pid, &detail, kill_error.as_ref()))
                    }
                }
            }
            #[cfg(not(windows))]
            Self::Std(mut child) => {
                let kill_error = child.start_kill().err();
                match bound_reap(kill_error, child.wait()).await {
                    Ok(status) => Ok(status),
                    Err((detail, kill_error)) => {
                        Err(residue_message(pid, &detail, kill_error.as_ref()))
                    }
                }
            }
        }
    }
}

fn spawn_service_owner(
    product_binary: &std::path::Path,
    service_root: &std::path::Path,
) -> io::Result<ServiceOwner> {
    // The product serves only `--port 0`: it binds and publishes its own port.
    let stderr = crate::server_log::open(service_root)?;
    let arguments = [
        std::ffi::OsStr::new("serve-service"),
        std::ffi::OsStr::new("--service"),
        service_root.as_os_str(),
        std::ffi::OsStr::new("--port"),
        std::ffi::OsStr::new("0"),
    ];
    #[cfg(windows)]
    {
        let mut launch = windows_spawn::Command::new(product_binary);
        for (name, _) in std::env::vars_os() {
            if grapt_service::launcher_environment::is_launcher_identity(&name) {
                launch.env_remove(name);
            }
        }
        launch
            .args(arguments)
            .stdin(windows_spawn::Stdio::null())
            .stdout(windows_spawn::Stdio::null())
            .stderr(stderr)
            .spawn()
            .map(ServiceOwner::Listed)
    }
    #[cfg(not(windows))]
    {
        let mut launch = tokio::process::Command::new(product_binary);
        grapt_service::launcher_environment::scrub(launch.as_std_mut());
        launch
            .args(arguments)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::from(stderr))
            .kill_on_drop(false)
            .spawn()
            .map(ServiceOwner::Std)
    }
}

/// `service status` and `service start` render as the TypeScript CLI did: the
/// service URL on one line, or `stopped` when no live service is registered.
async fn service(
    action: ServiceAction,
    service_root: std::path::PathBuf,
    product_binary: Option<std::path::PathBuf>,
    limits: CliLimits,
) -> u8 {
    let mut sink = output_sink(OutputMode::Text, limits);
    let rendered = match (action, product_binary) {
        (ServiceAction::Start, Some(product_binary)) => {
            match start_service(service_root, product_binary).await {
                Ok(service) => service.endpoint().to_owned(),
                Err((class, message)) => return diagnostic_exit(&mut sink, class, &message),
            }
        }
        (ServiceAction::Start, None) => {
            return diagnostic_exit(
                &mut sink,
                ExitClass::Usage,
                &"service start requires --product",
            );
        }
        (ServiceAction::Status, _) => {
            let discovered =
                match ServiceDiscovery::try_new(service_root, env!("CARGO_PKG_VERSION")) {
                    Ok(discovery) => discovery.discover().await,
                    Err(error) => return diagnostic_exit(&mut sink, ExitClass::Discovery, &error),
                };
            match discovered {
                Ok(service) => service.endpoint().to_owned(),
                Err(DiscoveryError::Absent | DiscoveryError::Stale(_)) => "stopped".to_owned(),
                Err(error) => return diagnostic_exit(&mut sink, ExitClass::Discovery, &error),
            }
        }
    };
    match sink.text(&rendered) {
        Ok(()) => ExitClass::Success.code(),
        Err(error) => output_failure(&mut sink, error),
    }
}

const USAGE: &str = "\
usage: grapt <command> [options]

commands:
  recover-sign-in --profile <profile> [--restore-legacy] (Windows)
  update  --from <package-directory> [--prefix <installation>]
  update  --prune [--prefix <installation>]
  revert  [--prefix <installation>] [--profile <profile>]
  uninstall [--prefix <installation>]
  code    [--profile <profile>] [--setup <account-directory>] [repository]
          start coding: the native terminal and its own product together
  login   <provider> [--profile <profile>] [--no-browser | --key-stdin | --api-key-env <name>] [--json]
  logout  <connection> [--profile <profile>] [--json]
  auth status [--profile <profile>] [--json]
          connect, inspect or disconnect model providers
  status  --service <root> [--json|--jsonl]
  launch  --service <root> --product <binary> [--json|--jsonl]
  service status --service <root>
  service start  --service <root> --product <binary>
  session rename <id> <title> [--service <root> | --remote <https-url> --credential <file>]
          rename an existing Session; defaults to the running default-profile service
  session provision --service <root> --session <id> --agent <id> --model <provider/model>
          --location <directory> --identity-entry <json> --packet-entry <json>
          read the issued StateEngine capability from stdin; requires a running service
  attach  <connection> (--session <id> | --resume | --new) [--project <id>] [--json|--jsonl]
  run     <connection> [--session <id> | -s <id> | --resume | --continue | -c | --new]
          (<message>... | --prompt <text> | --stdin) [--format default | --json | --jsonl] [--thinking] [--reasoning-effort <level>]
          [--input-id <msg_KEY>] [--background] [--worktree <new-directory>]
  plugin marketplace add <source> [--service <root>]
  plugin install <name@marketplace> [--scope user] [--service <root>]
          install unchanged local Claude prompt skills and commands
  mcp add    <server> --service <root> [--env KEY=VALUE] (--url <url> | -- <command> [args...])
  mcp remove <server> --service <root>
  mcp list   --service <root> [--directory <repository>] [--json]
  mcp login  <server> --service <root> [--client-id <id>] [--scope <scopes>] [--no-browser]
  mcp logout <server> --service <root>
             [--directory <repository>] selects the MCP configuration location
  import mcp --from <claude|codex> --service <root> [--directory <repository>] [--apply]
  import <instructions|skills|hooks> --from <claude|codex> [--service <root>] [--directory <repository>] [--apply]
             copy MCP server definitions into Grapt configuration; dry run by default
  acp     <connection>

connection: --service <root> | --remote <https-url> --credential <file>
            | --embedded --service <root> --product <binary>

Stable exit codes:
  0 success; 1 auth status has no usable connection; 2 invalid command usage
  3 not found (including a logout connection not signed in); 4 operation refused
  10 service discovery failed; 11 incompatible protocol; 12 remote unavailable
  13 authentication failed; 71 child failed; 74 output failed; 130 cancelled
HTTP 404 exits 3; HTTP 401 exits 13; other HTTP 4xx exit 4.
HTTP 5xx and request transport failures exit 12.

Run grapt code --help for first-run setup.
Run grapt update --help for installer options, including Windows portable installs.
See docs/cli-compatibility/MATRIX.md for
the TypeScript CLI surfaces Grapt refuses and why.
";

pub fn command_error_exit(error: &impl core::fmt::Display) -> u8 {
    let mut sink = output_sink(OutputMode::Text, CliLimits::production());
    diagnostic_exit(&mut sink, ExitClass::Usage, error)
}

fn remote(error: crate::ApiClientError) -> (ExitClass, String) {
    (error.exit_class(), error.to_string())
}

fn output_sink(mode: OutputMode, limits: CliLimits) -> BoundedOutput<io::Stdout, io::Stderr> {
    BoundedOutput::new(io::stdout(), io::stderr(), mode, limits)
}

/// One bounded text line to the operator. An unconsumed output pipe cannot
/// block signal handling or hold the command forever; the binary exits
/// without joining a refused blocking writer.
pub(super) async fn emit(
    message: String,
    diagnostic: bool,
    limits: CliLimits,
) -> Result<(), String> {
    let writer = tokio::task::spawn_blocking(move || {
        let mut output = output_sink(OutputMode::Text, limits);
        if diagnostic {
            output.diagnostic(&message)
        } else {
            output.text(&message)
        }
    });
    match tokio::time::timeout(Duration::from_secs(5), writer).await {
        Ok(Ok(Ok(()))) => Ok(()),
        _ => Err("command output could not be written".to_owned()),
    }
}

/// A JSONL stream ends with the run receipt, so a script resuming a Session
/// reads the exact Session identity and exit class it acted on. `--json`
/// keeps its single response document.
fn receipt_exit<O: Write, E: Write>(output: &mut BoundedOutput<O, E>, receipt: &RunReceipt) -> u8 {
    if matches!(output.mode(), OutputMode::Jsonl) {
        if let Err(error) = output.machine(receipt) {
            return output_failure(output, error);
        }
    }
    receipt.exit.code()
}

fn diagnostic_exit<O: Write, E: Write>(
    output: &mut BoundedOutput<O, E>,
    class: ExitClass,
    error: &impl core::fmt::Display,
) -> u8 {
    if output.diagnostic(&format!("grapt: {error}")).is_err() {
        ExitClass::Output.code()
    } else {
        class.code()
    }
}

fn output_failure<O: Write, E: Write>(output: &mut BoundedOutput<O, E>, error: OutputError) -> u8 {
    if !matches!(error, OutputError::BrokenPipe) {
        let _diagnostic_result = output.diagnostic(&format!("grapt: {error}"));
    }
    ExitClass::Output.code()
}

fn emit_service<O: Write, E: Write>(
    output: &mut BoundedOutput<O, E>,
    service: &crate::DiscoveredService,
    verb: &str,
) -> Result<(), OutputError> {
    match output.mode() {
        OutputMode::Text => output.text(&format!("{verb} {}", service.endpoint())),
        OutputMode::Json | OutputMode::Jsonl => output.machine(&service.status_receipt()),
    }
}

async fn read_stdin(limits: CliLimits) -> Result<String, String> {
    let bytes = read_stdin_bytes(limits).await?;
    String::from_utf8(bytes).map_err(|_| "stdin is not UTF-8".to_owned())
}

async fn read_stdin_bytes(limits: CliLimits) -> Result<Vec<u8>, String> {
    let maximum = limits.values().stdin_bytes;
    let take = u64::try_from(maximum)
        .map_err(|_| "stdin bound does not fit the platform".to_owned())?
        .saturating_add(1);
    let mut bytes = Vec::new();
    tokio::io::stdin()
        .take(take)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| format!("stdin read failed: {error}"))?;
    if bytes.len() > maximum {
        return Err(format!("stdin exceeded its {maximum}-byte bound"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod residue_tests {
    use super::residue_message;

    #[test]
    fn the_named_residue_carries_the_pid_the_bound_and_the_kill_refusal() {
        let kill_error = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        assert_eq!(
            residue_message(
                Some(4242),
                "it was not reaped within its 10-second cleanup bound",
                Some(&kill_error)
            ),
            "the exact child (pid 4242) - it was not reaped within its 10-second cleanup bound; the kill itself was refused: access denied"
        );
        assert_eq!(
            residue_message(None, "it was not reaped: ECHILD", None),
            "the exact child - it was not reaped: ECHILD"
        );
    }

    #[test]
    fn the_named_residue_carries_the_raw_errno_refusal_shape() {
        // The kill refusal is carried verbatim, whatever its errno.
        let raw_errno = std::io::Error::other("No such process (os error 38)");
        assert_eq!(
            residue_message(
                Some(31337),
                "it was not reaped within its 10-second cleanup bound",
                Some(&raw_errno)
            ),
            "the exact child (pid 31337) - it was not reaped within its 10-second cleanup bound; the kill itself was refused: No such process (os error 38)"
        );
    }
}

#[cfg(test)]
mod bounded_reap_tests;

#[cfg(all(test, unix))]
mod server_log_tests {
    #[tokio::test]
    async fn service_owner_stderr_and_failure_use_the_profile_log(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = crate::testfs::ScratchDir::new("owner-log")?;
        let product = root.join("product");
        crate::testfs::write_executable(
            &product,
            b"#!/bin/sh\necho owner-server-diagnostic >&2\nexit 3\n",
        )?;
        let outcome = super::start_service(root.to_path_buf(), product).await;
        let bytes = std::fs::read(root.join("logs/server.log"))?;
        let (_, message) = outcome.err().ok_or("stub server must refuse")?;
        assert!(message.contains("exit status: 3"));
        assert!(message.contains(&format!(
            "server log: {}",
            root.join("logs/server.log").display()
        )));
        assert_eq!(bytes, b"owner-server-diagnostic\n");
        Ok(())
    }
}
