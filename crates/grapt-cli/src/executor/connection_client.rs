//! One-shot operators borrow a running service or own its ordinary terminal lifetime,
//! on a named profile or the user's default one.
use super::*;
use std::path::{Path, PathBuf};

type Failure = (ExitClass, String);

pub(super) async fn open(
    root: &Path,
    create: bool,
    limits: CliLimits,
) -> Result<Option<ApiClient>, Failure> {
    let present = match std::fs::symlink_metadata(root) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err((
                ExitClass::Discovery,
                format!("profile is unreadable: {:?}", error.kind()),
            ))
        }
    };
    if !present && !create {
        return Ok(None);
    }
    let discovery = ServiceDiscovery::try_new(root.to_path_buf(), env!("CARGO_PKG_VERSION"))
        .map_err(|error| (ExitClass::Discovery, error.to_string()))?;
    if present {
        match discovery.discover().await {
            Ok(service) => {
                return ApiClient::from_discovered(&service, limits)
                    .map(Some)
                    .map_err(|error| (ExitClass::Protocol, error.to_string()))
            }
            Err(
                crate::DiscoveryError::Absent
                | crate::DiscoveryError::ReadyAbsent
                | crate::DiscoveryError::Stale(_),
            ) => (),
            Err(error) => return Err((ExitClass::Discovery, error.to_string())),
        }
    }
    let binary = product_binary()?;
    if !present {
        crate::client::terminal::create_profile_parent(root)
            .map_err(|reason| (ExitClass::Usage, reason))?;
    }
    // A missing profile must be created by the product before log preparation.
    let result = super::embedded_client_mode(
        binary,
        root.to_path_buf(),
        limits,
        "serve-connections",
        present,
    )
    .await;
    match result {
        Ok(client) => Ok(Some(client)),
        // An incumbent may have won election between discovery and launch:
        // borrow it once it is ready. There is no write or launch retry.
        Err(original) => borrow_winner(&discovery, limits)
            .await
            .map_or(Err(original), |client| client.map(Some)),
    }
}

/// A command that names no connection: the user's default profile, through
/// its running service or one this command owns until it exits.
pub(super) async fn default_profile(limits: CliLimits) -> Result<ApiClient, Failure> {
    let root = crate::command::terminal::user_profile(|name| std::env::var_os(name)).ok_or((
        ExitClass::Usage,
        "default profile is unavailable; provide --service <root>".to_owned(),
    ))?;
    match std::fs::symlink_metadata(&root) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let detected = super::connections::ambient_logins();
            let step = if detected.is_empty() {
                "connect one with grapt login <provider>, for example grapt login ollama or grapt login anthropic".to_owned()
            } else {
                format!("connect one with {}", detected.join(" or "))
            };
            return Err((
                ExitClass::Usage,
                format!(
                    "no provider is connected yet (no profile at {}); {step}",
                    root.display()
                ),
            ));
        }
        Err(error) => {
            return Err((
                ExitClass::Discovery,
                format!("profile is unreadable: {:?}", error.kind()),
            ))
        }
    }
    let discovery = ServiceDiscovery::try_new(root.clone(), env!("CARGO_PKG_VERSION"))
        .map_err(|error| (ExitClass::Discovery, error.to_string()))?;
    match discovery.discover().await {
        Ok(service) => {
            return ApiClient::from_discovered(&service, limits)
                .map_err(|error| (ExitClass::Protocol, error.to_string()))
        }
        Err(
            crate::DiscoveryError::Absent
            | crate::DiscoveryError::ReadyAbsent
            | crate::DiscoveryError::Stale(_),
        ) => {}
        Err(error) => return Err((ExitClass::Discovery, error.to_string())),
    }
    match super::embedded_client(product_binary()?, root, limits).await {
        Ok(client) => Ok(client),
        // Another command may have won the profile's election between the
        // discovery above and this launch: borrow its service. There is no
        // launch retry.
        Err(original) => borrow_winner(&discovery, limits)
            .await
            .unwrap_or(Err(original)),
    }
}

/// How long a command that lost the profile's election waits, in total, for
/// the winner's service to finish starting: five seconds of starts, then time
/// for a probe begun then to finish its authenticated health check's connect,
/// write and read.
const BORROW_WINDOW: Duration =
    Duration::from_secs(5).saturating_add(grapt_service::HEALTH_PROBE_ALLOWANCE);
/// The first pause between discovery attempts; each later one doubles, up to
/// one second.
const BORROW_FIRST_WAIT: Duration = Duration::from_millis(100);

/// The winning service, once it answers within the window. The winner holds
/// the profile's lock before it writes its registration, then its ready
/// receipt, then answers health, so each of those gaps is a start in
/// progress and is asked again. Any other discovery error ends the wait, as
/// does the window, and the launch's own failure stands.
async fn borrow_winner(
    discovery: &ServiceDiscovery,
    limits: CliLimits,
) -> Option<Result<ApiClient, Failure>> {
    let service = poll_ready(BORROW_WINDOW, || async {
        match discovery.discover().await {
            Ok(service) => Attempt::Ready(service),
            Err(error) if error.is_launch_pending() => Attempt::Pending,
            Err(_) => Attempt::Stop,
        }
    })
    .await?;
    Some(
        ApiClient::from_discovered(&service, limits)
            .map_err(|error| (ExitClass::Protocol, error.to_string())),
    )
}

/// One readiness probe's outcome.
enum Attempt<T> {
    Ready(T),
    Pending,
    Stop,
}

/// Probe until ready, stopped or `window` has passed, with rising pauses
/// between probes. Every probe, the last included, is bounded by what is left
/// of the window: one still running at its end is abandoned, and an answer
/// that comes after it is not used.
async fn poll_ready<T, F, P>(window: Duration, mut probe: F) -> Option<T>
where
    F: FnMut() -> P,
    P: std::future::Future<Output = Attempt<T>>,
{
    let deadline = tokio::time::Instant::now() + window;
    let mut wait = BORROW_FIRST_WAIT;
    loop {
        let attempt = tokio::time::timeout_at(deadline, probe()).await.ok()?;
        let now = tokio::time::Instant::now();
        match attempt {
            Attempt::Ready(value) if now <= deadline => return Some(value),
            Attempt::Ready(_) | Attempt::Stop => return None,
            Attempt::Pending if now >= deadline => return None,
            Attempt::Pending => {}
        }
        tokio::time::sleep_until((now + wait).min(deadline)).await;
        // A probe begun at the deadline would still run its first,
        // synchronous step before its timeout is checked.
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        wait = (wait * 2).min(Duration::from_secs(1));
    }
}

fn product_binary() -> Result<PathBuf, Failure> {
    let binary = std::env::current_exe().map_err(|_| {
        (
            ExitClass::Child,
            "CLI installation path is unavailable".into(),
        )
    })?;
    let directory = binary.parent().ok_or((
        ExitClass::Child,
        "CLI installation directory is unavailable".into(),
    ))?;
    let product = directory.join(if cfg!(windows) {
        "grapt-server.exe"
    } else {
        "grapt-server"
    });
    if !product.is_file() {
        return Err((
            ExitClass::Child,
            "grapt-server must be installed beside grapt".into(),
        ));
    }
    Ok(product)
}

pub(super) async fn close(client: &ApiClient) -> Result<(), Failure> {
    let Some(mut child) = client.take_embedded_child().await else {
        return Ok(());
    };
    drop(child.stdin.take());
    if let Ok(Ok(status)) = tokio::time::timeout(Duration::from_secs(15), child.wait()).await {
        if status.success() {
            return Ok(());
        }
        return Err((
            ExitClass::Child,
            format!("connection service exited with {status}"),
        ));
    }
    let _stop = child.start_kill();
    tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .map_err(|_| {
            (
                ExitClass::Child,
                "connection service could not be reaped".into(),
            )
        })?
        .map_err(|_| {
            (
                ExitClass::Child,
                "connection service status is unavailable".into(),
            )
        })?;
    Err((
        ExitClass::Child,
        "connection service did not finish draining".into(),
    ))
}

#[cfg(test)]
mod readiness_tests;
