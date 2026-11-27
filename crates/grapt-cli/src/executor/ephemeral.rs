//! Volatile foreground transport: private pipes, never a discovery root or log.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::AsyncReadExt;

use super::{reap_embedded_failure, termination_signal};
use crate::{ApiClient, CliLimits, ConnectionChoice, ExitClass};

const READINESS_BYTES: usize = 16 * 1_024;
const READINESS_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) async fn close(owner: &ApiClient) -> Result<(), String> {
    match owner.take_embedded_child().await {
        Some(child) => super::acp::close_child(child).await,
        None => Ok(()),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Readiness {
    schema: String,
    pid: u32,
    endpoint: String,
    username: String,
    secret: String,
}

pub(super) async fn connect_ephemeral(
    connection: ConnectionChoice,
    limits: CliLimits,
) -> Result<ApiClient, (ExitClass, String)> {
    let ConnectionChoice::Embedded {
        product_binary,
        service_root,
    } = connection
    else {
        return Err((
            ExitClass::Usage,
            crate::CommandError::EphemeralTarget.to_string(),
        ));
    };
    // No creation, credential renewal, registration, log opening or profile
    // migration is permitted here. Product opens only existing provider inputs.
    existing_profile(&service_root)?;
    let mut launch = tokio::process::Command::new(product_binary);
    grapt_service::launcher_environment::scrub(launch.as_std_mut());
    let mut child = launch
        .arg("serve-ephemeral")
        .arg("--service")
        .arg(service_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            (
                ExitClass::Child,
                format!("ephemeral child launch failed: {error}"),
            )
        })?;
    let ready = tokio::select! {
        ready = tokio::time::timeout(READINESS_TIMEOUT, read_ready(&mut child)) => {
            match ready {
                Ok(ready) => ready,
                Err(_) => Err("ephemeral readiness exceeded its 30-second bound".to_owned()),
            }
        }
        _signal = termination_signal() => {
            Err("ephemeral launch cancelled before readiness".to_owned())
        }
    };
    let ready = match ready {
        Ok(ready) => ready,
        Err(error) => {
            return Err((ExitClass::Child, reap_embedded_failure(error, child).await));
        }
    };
    // Refuse a process that exited after publishing readiness, rather than
    // accidentally treating a reused endpoint as the owned child.
    match child.try_wait() {
        Ok(None) => {}
        Ok(Some(status)) => {
            return Err((
                ExitClass::Child,
                format!("ephemeral child exited: {status}"),
            ));
        }
        Err(error) => {
            return Err((
                ExitClass::Child,
                reap_embedded_failure(format!("ephemeral child status failed: {error}"), child)
                    .await,
            ));
        }
    }
    match ApiClient::from_ephemeral(
        &ready.endpoint,
        ready.pid,
        ready.username,
        ready.secret,
        child,
        limits,
    ) {
        Ok(client) => Ok(client),
        Err((error, child)) => Err((
            ExitClass::Protocol,
            reap_embedded_failure(error.to_string(), child).await,
        )),
    }
}

fn existing_profile(root: &Path) -> Result<(), (ExitClass, String)> {
    match std::fs::metadata(root) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        _ => Err((
            ExitClass::Usage,
            "--ephemeral --service must name an existing provider profile directory".to_owned(),
        )),
    }
}

async fn read_ready(child: &mut tokio::process::Child) -> Result<Readiness, String> {
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ephemeral child has no readiness pipe".to_owned())?;
    // A byte-at-a-time bounded read does not consume diagnostics or another
    // record after the one newline. No payload (including secrets) is echoed.
    let mut bytes = Vec::with_capacity(READINESS_BYTES);
    loop {
        let byte = stdout
            .read_u8()
            .await
            .map_err(|_| "ephemeral readiness ended before a complete JSON line".to_owned())?;
        if byte == b'\n' {
            break;
        }
        if bytes.len() == READINESS_BYTES {
            return Err("ephemeral readiness exceeded its finite byte bound".to_owned());
        }
        bytes.push(byte);
    }
    let ready: Readiness = serde_json::from_slice(&bytes)
        .map_err(|_| "ephemeral readiness is not the expected JSON record".to_owned())?;
    if ready.schema != "grapt.ephemeral-ready.v1" {
        return Err("ephemeral readiness schema is not supported".to_owned());
    }
    Ok(ready)
}
