//! Interactive process custody reuses the product's election, attributed
//! discovery and prompt drain. This client never elects a service or owns facts.

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

use crate::command::terminal::TerminalCommand;
use crate::{CommandFailure, ServiceDiscovery};

mod attached;
mod control;
mod feedback;
mod first_run;
mod profile_trust;
mod ready;
mod secret_input;
mod selection;

/// Share the established hidden terminal reader with the login operator.
pub(crate) fn read_api_key(permits_empty: bool) -> Result<String, String> {
    let prompt = if permits_empty {
        "API key (hidden; Enter for no key): "
    } else {
        "API key (hidden): "
    };
    secret_input::read_secret(prompt, permits_empty)
        .map_err(|reason| reason.replace("; nothing was created", ""))
}

const PRODUCT_DRAIN: Duration = Duration::from_secs(20);
const REAP: Duration = Duration::from_secs(5);

pub async fn run(command: TerminalCommand) -> Result<u8, CommandFailure> {
    let binary = std::env::current_exe()
        .map_err(|_| "cannot locate the extracted installation".to_owned())?;
    let directory = binary
        .parent()
        .ok_or("the installation has no binary directory")?;
    run_with_binaries(
        command,
        &sibling(directory, "grapt-server"),
        &sibling(directory, "grapt-tui"),
    )
    .await
}

fn sibling(directory: &Path, name: &str) -> PathBuf {
    directory.join(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    })
}

async fn run_with_binaries(
    command: TerminalCommand,
    product: &Path,
    terminal: &Path,
) -> Result<u8, CommandFailure> {
    let profile = command.profile.clone();
    run_with_binaries_inner(command, product, terminal)
        .await
        .map_err(|error| {
            CommandFailure::new(
                error.exit_class(),
                crate::server_log::failure(&profile, error.to_string()),
            )
        })
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn run_with_binaries_inner(
    command: TerminalCommand,
    product: &Path,
    terminal: &Path,
) -> Result<u8, CommandFailure> {
    let repository = std::fs::canonicalize(&command.repository).map_err(|error| {
        format!(
            "repository cannot be opened: {:?}; choose a readable directory",
            error.kind()
        )
    })?;
    if !repository.is_dir() {
        return Err("repository must be a directory".into());
    }
    let directory = repository
        .to_str()
        .ok_or("repository must be a UTF-8 path")?;
    let approval = match profile_trust::authorize(&command, &repository)? {
        profile_trust::Authorization::Revoked => return Ok(0),
        profile_trust::Authorization::Launch(approval) => approval,
    };
    if let Some(source) = &command.setup {
        create_profile_parent(&command.profile)?;
        initialize_profile(product, &command.profile, Some(source)).await?;
    } else if std::fs::symlink_metadata(&command.profile)
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        && first_run::interactive()
    {
        first_run::create_profile(product, &command.profile).await?;
    }
    match std::fs::symlink_metadata(&command.profile) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err("profile must be a real directory; choose a supported profile path".into());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "No profile is configured at {}. Run grapt code in a terminal to connect a provider, or grapt code --setup <account-directory> to import an existing account",
                command.profile.display()
            ).into());
        }
        Err(error) => {
            return Err(format!(
                "profile is unreadable: {:?}; repair access before coding",
                error.kind()
            )
            .into());
        }
    }
    // Configuration discovery/precedence belongs to PlatformOwner. A missing
    // local grapt.jsonc may legitimately inherit a configured ancestor.
    for file in ["service.conf", "basic.credential"] {
        let path = command.profile.join(file);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => {
                return Err(
                    format!("profile {file} must be a regular file; repair the profile").into(),
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!(
                    "profile {file} is missing; repair this profile or use --setup with an unused profile directory"
                ).into());
            }
            Err(error) => {
                return Err(format!(
                    "profile {file} is unreadable: {:?}; repair access before coding",
                    error.kind()
                )
                .into());
            }
        }
    }
    let discovery = ServiceDiscovery::try_new(&command.profile, env!("CARGO_PKG_VERSION"))
        .map_err(|error| error.to_string())?;
    match discovery.discover().await {
        Ok(ready) => return attached::run(&command, terminal, directory, ready).await,
        // The product's election still decides whether a stale registration
        // can be replaced. A client never removes or repairs it itself.
        Err(crate::DiscoveryError::Absent | crate::DiscoveryError::Stale(_)) => {}
        Err(error) => return Err(error.to_string().into()),
    }
    crate::client::chatgpt::renew_before_start(&command.profile).await?;
    let stderr = crate::server_log::open(&command.profile)
        .map_err(|error| format!("server log cannot be opened: {error}"))?;
    let mut launch = Command::new(product);
    grapt_service::launcher_environment::scrub(launch.as_std_mut());
    if let Some(approval) = approval {
        launch
            .arg("serve-terminal")
            .arg("--profile-approval")
            .arg(approval);
    } else {
        launch.arg("serve-terminal");
    }
    let mut child = launch
        .arg("--service")
        .arg(&command.profile)
        .arg("--port")
        .arg("0")
        .arg("--notify-ready")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            format!(
                "product cannot start: {:?}; extract all installation binaries together",
                error.kind()
            )
        })?;
    let pid = match child.id() {
        Some(pid) => pid,
        None => {
            force_reap(&mut child).await?;
            return Err("product started without an observable process identity".into());
        }
    };
    let ready = ready::start(&mut child, &discovery, pid).await?;
    // Keep the receipt pipe open: the product writes its bounded final receipt
    // during drain. Closing it early would turn a clean shutdown into a broken pipe.
    let health = match ready.product_health().and_then(|health| {
        health
            .encoded()
            .map_err(|_| crate::DiscoveryError::ReadyMismatch)
    }) {
        Ok(health) => health,
        Err(error) => {
            drain(&mut child).await?;
            return Err(error.to_string().into());
        }
    };
    println!(
        "{}",
        crate::operator_text::opening_repository(directory, cfg!(windows))
    );
    let mut terminal_launch =
        match selection::launch(&command, terminal, directory, &health, &ready).await {
            Ok(launch) => launch,
            Err(error) => {
                drain(&mut child).await?;
                return Err(error);
            }
        };
    let mut ui = match terminal_launch
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(ui) => ui,
        Err(error) => {
            drain(&mut child).await?;
            return Err(format!(
                "terminal cannot start: {:?}; extract grapt-tui beside grapt",
                error.kind()
            )
            .into());
        }
    };
    // Tokio Child::wait closes its own stdin before polling for exit. Hold
    // this private control pipe separately so observing loss cannot SEND EOF.
    let control = match control::retain(&mut child) {
        Ok(control) => control,
        Err(error) => {
            let product_cleanup = force_reap(&mut child).await;
            let terminal_cleanup = finish_terminal(&mut ui).await;
            product_cleanup?;
            terminal_cleanup?;
            return Err(error.into());
        }
    };
    enum End {
        Terminal(Result<ExitStatus, std::io::Error>),
        Product(Result<ExitStatus, std::io::Error>),
        Cancel(Result<(), std::io::Error>),
    }
    let end = tokio::select! {
        status = ui.wait() => End::Terminal(status),
        status = control::observe(&mut child, control) => End::Product(status),
        signal = tokio::signal::ctrl_c() => End::Cancel(signal),
    };
    match end {
        End::Terminal(status) => {
            let product_status = drain(&mut child).await?;
            let status = status
                .map_err(|error| format!("terminal exit is unreadable: {:?}", error.kind()))?;
            if !product_status.success() {
                return Err(format!(
                    "product drain failed ({product_status}); restart to reconcile durable history"
                )
                .into());
            }
            Ok(if status.success() { 0 } else { 1 })
        }
        End::Product(status) => {
            // Give the real TUI time to report loss and restore its terminal.
            finish_terminal(&mut ui).await?;
            let status = status
                .map_err(|error| format!("product exit is unreadable: {:?}", error.kind()))?;
            Err(format!(
                "product exited during coding ({status}); restart to resume durable history"
            )
            .into())
        }
        End::Cancel(signal) => {
            let product_cleanup = drain(&mut child).await;
            let terminal_cleanup = finish_terminal(&mut ui).await;
            product_cleanup?;
            terminal_cleanup?;
            signal.map_err(|error| format!("cancellation observer failed: {:?}", error.kind()))?;
            Ok(130)
        }
    }
}

const TERMINAL_WINDOW: &str =
    "product did not publish its own readiness within the launch allowance";

/// Appends a cleanup failure without losing the original refusal.
fn preserved_refusal_with_cleanup_error(refusal: &str, cleanup_error: &str) -> String {
    format!("{refusal}; exact-child cleanup also failed: {cleanup_error}")
}

fn startup_feedback(child_pid: u32, error: crate::ReadinessWait) -> String {
    match error {
        crate::ReadinessWait::Child(reason) => format!("product startup failed: {reason}"),
        crate::ReadinessWait::StatusFailed(reason) => format!("product startup failed: {reason}"),
        crate::ReadinessWait::Discovery(reason) => format!(
            "product startup attribution failed: {reason}; repair the profile before trying again"
        ),
        crate::ReadinessWait::Window(observation) => format!(
            "{}; check the installation and profile before trying again",
            crate::executor::window_refusal(TERMINAL_WINDOW, child_pid, &observation)
        ),
    }
}

/// Setup creates the profile itself, but the user configuration directory it
/// lives in may not exist yet on a fresh machine. It is private to the user.
pub(crate) fn create_profile_parent(profile: &Path) -> Result<(), String> {
    let Some(parent) = profile.parent() else {
        return Ok(());
    };
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(parent).map_err(|error| {
        format!(
            "profile directory {} cannot be created: {:?}; name another with --profile",
            parent.display(),
            error.kind()
        )
    })
}

async fn initialize_profile(
    product: &Path,
    profile: &Path,
    source: Option<&Path>,
) -> Result<(), String> {
    let mut command = Command::new(product);
    command.arg("init-profile").arg("--service").arg(profile);
    if let Some(source) = source {
        command.arg("--from").arg(source);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            format!(
                "setup product cannot start: {:?}; extract all binaries together",
                error.kind()
            )
        })?;
    let result = tokio::select! {
        result = tokio::time::timeout(PRODUCT_DRAIN, async {
        let stderr = child.stderr.take().ok_or("setup feedback pipe is absent")?;
        let mut bytes = Vec::new();
        stderr
            .take(8_193)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "setup feedback is unreadable")?;
        if bytes.len() > 8_192 {
            return Err("setup feedback exceeded its bound".to_owned());
        }
        let status = child.wait().await.map_err(|_| "setup exit is unreadable")?;
        if !status.success() {
            let feedback = String::from_utf8(bytes).map_err(|_| "setup feedback is invalid")?;
            return Err(format!(
                "{}; existing account inputs were not changed; preserve any partial new profile",
                feedback.trim()
            ));
        }
        Ok(())
        }) => result,
        signal = tokio::signal::ctrl_c() => {
            force_reap(&mut child).await?;
            signal.map_err(|_| "setup cancellation observer failed".to_owned())?;
            return Err("setup cancelled; preserve any partial new profile and choose another directory".into());
        }
    };
    match result {
        Ok(Ok(())) => {
            if source.is_some() {
                println!("Existing account configured in {}", profile.display());
            } else {
                println!("Profile created. Connect a provider inside Grapt.");
            }
            Ok(())
        }
        Ok(Err(error)) => {
            force_reap(&mut child).await?;
            Err(error)
        }
        Err(_) => {
            force_reap(&mut child).await?;
            Err("setup exceeded its allowance; preserve any partial new profile and choose another directory".into())
        }
    }
}

async fn drain(child: &mut Child) -> Result<ExitStatus, String> {
    // EOF is sent only to our child's private pipe, never to an incumbent.
    drop(child.stdin.take());
    match tokio::time::timeout(PRODUCT_DRAIN, child.wait()).await {
        Ok(status) => {
            status.map_err(|error| format!("owned product exit is unreadable: {:?}", error.kind()))
        }
        Err(_) => {
            force_reap(child).await?;
            Err("product exceeded its drain allowance and was reaped; restart to reconcile interrupted work".into())
        }
    }
}

fn terminal_command(
    terminal: &Path,
    profile: &Path,
    directory: &str,
    health: &str,
    preset: Option<&str>,
    effort: Option<&str>,
    session: Option<&grapt_types::SessionId>,
) -> Command {
    let mut command = Command::new(terminal);
    command.arg("--service").arg(profile);
    if let Some(session) = session {
        command.arg("--session").arg(session.as_str());
    } else {
        command.args(["--directory", directory]);
    }
    command
        .arg("--expect-health")
        .arg(health)
        .args(preset.into_iter().map(|name| {
            if name == "bypass" {
                "--dangerously-bypass-permissions".to_owned()
            } else {
                format!("--{name}")
            }
        }));
    if let Some(effort) = effort {
        command.args(["--reasoning-effort", effort]);
    }
    command
}

async fn finish_terminal(child: &mut Child) -> Result<(), String> {
    match tokio::time::timeout(PRODUCT_DRAIN, child.wait()).await {
        Ok(status) => {
            status.map_err(|error| format!("terminal exit is unreadable: {:?}", error.kind()))?;
            Ok(())
        }
        Err(_) => {
            force_reap(child).await?;
            Err("terminal exceeded its exit allowance and was reaped; restore terminal settings before restarting".into())
        }
    }
}

async fn force_reap(child: &mut Child) -> Result<(), String> {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return Ok(());
    }
    child
        .start_kill()
        .map_err(|error| format!("owned process termination failed: {:?}", error.kind()))?;
    tokio::time::timeout(REAP, child.wait())
        .await
        .map_err(|_| "owned process could not be reaped within its allowance".to_owned())?
        .map_err(|error| format!("owned process exit is unreadable: {:?}", error.kind()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{preserved_refusal_with_cleanup_error, startup_feedback};
    use crate::{DiscoveryError, LaunchWindowObservation, ReadinessWait};

    #[test]
    fn terminal_launch_forwards_only_explicit_trust_without_changing_profile_or_health() {
        use std::path::Path;
        for preset in [
            None,
            Some("inspect"),
            Some("author"),
            Some("maintainer"),
            Some("bypass"),
        ] {
            let command = super::terminal_command(
                Path::new("terminal"),
                Path::new("profile"),
                "repository",
                "pinned-health",
                preset,
                Some("high"),
                None,
            );
            let args: Vec<_> = command
                .as_std()
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            let mut expected = vec![
                "--service",
                "profile",
                "--directory",
                "repository",
                "--expect-health",
                "pinned-health",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
            if let Some(preset) = preset {
                expected.push(if preset == "bypass" {
                    "--dangerously-bypass-permissions".to_owned()
                } else {
                    format!("--{preset}")
                });
            }
            expected.extend(["--reasoning-effort".to_owned(), "high".to_owned()]);
            assert_eq!(args, expected);
        }
    }

    /// One run of the file-capture witness in its own scratch directory. The
    /// refusal is returned rather than asserted so the fork-load witness can
    /// tell an `ExecutableFileBusy` start apart from any other failure.
    #[cfg(unix)]
    async fn file_capture_once() -> Result<(), String> {
        let root =
            crate::testfs::ScratchDir::new("terminal-log").map_err(|error| error.to_string())?;
        file_capture_in(&root).await
    }

    #[cfg(unix)]
    async fn file_capture_in(root: &std::path::Path) -> Result<(), String> {
        let fixture = |result: std::io::Result<()>| result.map_err(|error| error.to_string());
        fixture(std::fs::write(root.join("service.conf"), b"fixture"))?;
        fixture(std::fs::write(
            root.join("basic.credential"),
            b"fixture:inert",
        ))?;
        let product = root.join("product");
        fixture(crate::testfs::write_executable(
            &product,
            b"#!/bin/sh\necho terminal-server-diagnostic >&2\necho '{\"schema\":\"grapt.product.launch.v1\",\"outcome\":\"refused\",\"reason\":\"fixture startup refusal\"}'\nexit 3\n",
        ))?;
        let command = super::TerminalCommand {
            profile: root.to_path_buf(),
            profile_source: crate::command::terminal::ProfileSource::Explicit,
            user_profile: None,
            profile_trust: Default::default(),
            repository: root.to_path_buf(),
            setup: None,
            preset: None,
            reasoning_effort: None,
            selection: Default::default(),
        };
        let outcome = super::run_with_binaries(command, &product, &root.join("unused-tui")).await;
        let message = outcome.err().ok_or("stub server must refuse")?.to_string();
        if !message.contains("fixture startup refusal") {
            return Err(format!("stdout refusal receipt was lost: {message}"));
        }
        if !message.contains(&format!(
            "server log: {}",
            root.join("logs/server.log").display()
        )) {
            return Err(format!("the server log is not named: {message}"));
        }
        let bytes =
            std::fs::read(root.join("logs/server.log")).map_err(|error| error.to_string())?;
        if bytes != b"terminal-server-diagnostic\n" {
            return Err(format!(
                "server log holds {:?}",
                String::from_utf8_lossy(&bytes)
            ));
        }
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_server_stderr_and_refusal_receipt_survive_file_capture() -> Result<(), String>
    {
        file_capture_once().await
    }

    /// The iteration count the fork-load stress witness runs. An explicit
    /// value is validated: a count of zero would let the witness pass while
    /// capturing nothing, and an unbounded count could pin a shared machine.
    #[cfg(unix)]
    fn fork_load_iterations(explicit: Option<String>) -> Result<usize, String> {
        const DEFAULT: usize = 50;
        const MAX: usize = 1_000;
        let Some(raw) = explicit else {
            return Ok(DEFAULT);
        };
        let value = raw
            .parse::<usize>()
            .map_err(|_| format!("GRAPT_FORK_LOAD_ITERATIONS={raw:?} is not a positive integer"))?;
        if value == 0 {
            return Err(
                "GRAPT_FORK_LOAD_ITERATIONS=0 runs no capture at all; the witness would prove nothing"
                    .to_owned(),
            );
        }
        if value > MAX {
            return Err(format!(
                "GRAPT_FORK_LOAD_ITERATIONS={value} exceeds the bound of {MAX}; lower the count"
            ));
        }
        Ok(value)
    }

    /// Guard rows for the stress count: zero, invalid and out-of-range values
    /// are refused loudly so the fork-load witness can never pass while
    /// capturing nothing.
    #[cfg(unix)]
    #[test]
    fn fork_load_iterations_rejects_zero_invalid_and_out_of_range_counts() {
        assert_eq!(fork_load_iterations(None), Ok(50));
        assert_eq!(fork_load_iterations(Some("1".to_owned())), Ok(1));
        assert_eq!(fork_load_iterations(Some("500".to_owned())), Ok(500));
        assert_eq!(fork_load_iterations(Some("1000".to_owned())), Ok(1_000));
        for rejected in ["0", "-1", "not-a-count", "1001"] {
            assert!(
                fork_load_iterations(Some(rejected.to_owned())).is_err(),
                "{rejected:?} must be refused"
            );
        }
    }

    /// The ETXTBSY flake's witness: the file-capture run, repeated while
    /// other threads keep forking, as the parallel test suite does. A fork
    /// taken while the fixture's write handle is open keeps a copy of the
    /// handle in the child's own table until that child execs, and a spawn
    /// of the file inside that interval fails with `ExecutableFileBusy`
    /// (ETXTBSY). Ignored by default; run with `--ignored` and optionally
    /// `GRAPT_FORK_LOAD_ITERATIONS` (default 50; values are validated to
    /// 1..=1000).
    #[cfg(unix)]
    #[ignore = "stress witness for the ETXTBSY race; run explicitly"]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn file_capture_survives_parallel_fork_load() -> Result<(), String> {
        let iterations = fork_load_iterations(std::env::var("GRAPT_FORK_LOAD_ITERATIONS").ok())?;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let forker_failures = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let forkers: Vec<_> = (0..8)
            .map(|_| {
                let stop = std::sync::Arc::clone(&stop);
                let forker_failures = std::sync::Arc::clone(&forker_failures);
                std::thread::spawn(move || {
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        if std::process::Command::new("/bin/true").status().is_err() {
                            forker_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                })
            })
            .collect();
        let mut busy = 0usize;
        let mut other = Vec::new();
        for _ in 0..iterations {
            match file_capture_once().await {
                Ok(()) => {}
                Err(reason) if reason.contains("ExecutableFileBusy") => busy += 1,
                Err(reason) => other.push(reason),
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for forker in forkers {
            let _joined = forker.join();
        }
        let forker_failures = forker_failures.load(std::sync::atomic::Ordering::Relaxed);
        println!(
            "fork-load iterations={iterations} executable_busy={busy} other_failures={} forker_failures={forker_failures}",
            other.len()
        );
        if busy == 0 && other.is_empty() && forker_failures == 0 {
            Ok(())
        } else {
            Err(format!(
                "{busy} of {iterations} runs hit ExecutableFileBusy; other failures: {other:?}; fork workers failed {forker_failures} spawns, so the load itself was not real"
            ))
        }
    }

    #[test]
    fn frc_startup_feedback_distinguishes_exit_unreadable_and_no_readiness() {
        let exited = startup_feedback(4242, ReadinessWait::Child("product exited (2)".into()));
        let unreadable = startup_feedback(
            4242,
            ReadinessWait::Discovery(DiscoveryError::Unreadable {
                path: "profile/serve-ready.json".into(),
                kind: std::io::ErrorKind::PermissionDenied,
            }),
        );
        let no_readiness = startup_feedback(
            4242,
            ReadinessWait::Window(LaunchWindowObservation::for_test(
                12,
                Some(DiscoveryError::ReadyAbsent),
            )),
        );
        let unknown_state = startup_feedback(
            4242,
            ReadinessWait::StatusFailed("product state is unreadable: UnexpectedEof".into()),
        );
        assert!(exited.contains("exited (2)"));
        assert!(unreadable.contains("unreadable"));
        assert!(unreadable.contains("PermissionDenied"));
        assert!(no_readiness.contains("process 4242 alive"));
        assert!(no_readiness.contains("did not publish its own readiness"));
        assert!(no_readiness.contains("12 polls"));
        assert!(no_readiness.contains("service ready receipt is absent"));
        assert!(unknown_state.contains("product state is unreadable"));
        assert_ne!(exited, unreadable);
    }

    #[test]
    fn frc_status_failed_cleanup_failure_preserves_the_original_refusal() {
        let refusal = "product startup failed: product state is unreadable: UnexpectedEof";
        let combined =
            preserved_refusal_with_cleanup_error(refusal, "it was not reaped: kill refused");
        assert!(combined.starts_with(refusal));
        assert!(
            combined.ends_with("exact-child cleanup also failed: it was not reaped: kill refused")
        );
        assert_ne!(combined, refusal);
    }
}
