//! A private readiness byte wakes attributed discovery without polling files.

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, ChildStdout};

use crate::discovery::LAUNCH_WINDOW;
use crate::{DiscoveredService, ReadinessWait, ServiceDiscovery};

struct Failure {
    reason: String,
    prefix: Option<u8>,
    force_reap: bool,
}

async fn signal(input: &mut (impl AsyncRead + Unpin)) -> Result<(), Failure> {
    let mut byte = [0];
    input.read_exact(&mut byte).await.map_err(|error| Failure {
        reason: format!(
            "product readiness pipe closed before notification: {:?}",
            error.kind()
        ),
        prefix: None,
        force_reap: false,
    })?;
    if byte[0] != 0x1e {
        return Err(Failure {
            reason: "product did not send its readiness notification".into(),
            // A startup refusal begins with JSON. Keep its first byte for the
            // existing bounded feedback decoder after draining the child.
            prefix: Some(byte[0]),
            force_reap: false,
        });
    }
    Ok(())
}

async fn discover(
    output: &mut Option<ChildStdout>,
    discovery: &ServiceDiscovery,
    pid: u32,
) -> Result<DiscoveredService, Failure> {
    let input = output.as_mut().ok_or_else(|| Failure {
        reason: "product readiness pipe is absent".into(),
        prefix: None,
        force_reap: false,
    })?;
    signal(input).await?;
    // The notification is only a wakeup. Registration, credential, health,
    // publication stability and exact-child ownership still decide readiness.
    discovery
        .discover_for_child(pid)
        .await
        .map_err(|error| Failure {
            reason: super::startup_feedback(pid, ReadinessWait::Discovery(error)),
            prefix: None,
            force_reap: false,
        })
}

pub(super) async fn start(
    child: &mut Child,
    discovery: &ServiceDiscovery,
    pid: u32,
) -> Result<DiscoveredService, String> {
    // Child::wait closes stdin. Hold the private control pipe independently
    // while observing process exit, then return both pipes to their owner.
    let control = child.stdin.take();
    let mut output = child.stdout.take();
    let deadline = tokio::time::Instant::now() + LAUNCH_WINDOW;
    let result = tokio::select! {
        biased;
        result = tokio::time::timeout_at(deadline, discover(&mut output, discovery, pid)) => {
            match result {
                Ok(result) if tokio::time::Instant::now() < deadline => result,
                _ => Err(Failure {
                reason: format!("{}; child pid {pid}", super::TERMINAL_WINDOW),
                prefix: None,
                force_reap: false,
                }),
            }
        }
        result = child.wait() => Err(Failure {
            reason: match &result {
                Ok(status) => format!("product exited before readiness ({status}); repair configuration or close the other profile owner"),
                Err(error) => format!("product state is unreadable: {:?}", error.kind()),
            },
            prefix: None,
            force_reap: result.is_err(),
        }),
        result = tokio::signal::ctrl_c() => Err(Failure {
            reason: match result {
                Ok(()) => "product startup cancelled".into(),
                Err(error) => format!("startup cancellation observer failed: {:?}", error.kind()),
            },
            prefix: None,
            force_reap: false,
        }),
    };
    child.stdin = control;
    child.stdout = output;
    match result {
        Ok(ready) => Ok(ready),
        Err(failure) => {
            // Keep stdin in the child until this explicit cleanup: waiting for
            // process exit earlier would send EOF and shut the product down.
            let cleanup = if failure.force_reap {
                super::force_reap(child).await
            } else {
                super::drain(child).await.map(|_| ())
            };
            let refusal = super::feedback::startup(child, failure.reason, failure.prefix).await;
            Err(match cleanup {
                Ok(_) => refusal,
                Err(error) => super::preserved_refusal_with_cleanup_error(&refusal, &error),
            })
        }
    }
}

#[cfg(test)]
mod tests;
