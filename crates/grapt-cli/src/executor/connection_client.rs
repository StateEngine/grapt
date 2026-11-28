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
        Err(original) => {
            // An incumbent may have won election between discovery and launch.
            // Attribution is repeated once; there is no write or launch retry.
            match discovery.discover().await {
                Ok(service) => ApiClient::from_discovered(&service, limits)
                    .map(Some)
                    .map_err(|error| (ExitClass::Protocol, error.to_string())),
                Err(_) => Err(original),
            }
        }
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

/// How long, in rising steps, a command that lost the profile's election
/// waits for the winner's service to finish starting: about five seconds.
const BORROW_WAITS: [Duration; 8] = [
    Duration::from_millis(100),
    Duration::from_millis(200),
    Duration::from_millis(400),
    Duration::from_millis(800),
    Duration::from_secs(1),
    Duration::from_secs(1),
    Duration::from_secs(1),
    Duration::from_secs(1),
];

/// The winning service, once it answers. A registered service whose ready
/// receipt or health is not there yet is still starting, so it is asked again;
/// no registration at all means no winner, and the launch's own failure stands.
async fn borrow_winner(
    discovery: &ServiceDiscovery,
    limits: CliLimits,
) -> Option<Result<ApiClient, Failure>> {
    for wait in BORROW_WAITS {
        match discovery.discover().await {
            Ok(service) => {
                return Some(
                    ApiClient::from_discovered(&service, limits)
                        .map_err(|error| (ExitClass::Protocol, error.to_string())),
                )
            }
            Err(crate::DiscoveryError::ReadyAbsent | crate::DiscoveryError::Stale(_)) => {
                tokio::time::sleep(wait).await;
            }
            Err(_) => return None,
        }
    }
    None
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
