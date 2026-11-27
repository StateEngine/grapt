//! A discovered service owns its lifetime independently of this terminal.

use std::path::Path;
use std::process::Stdio;

use crate::command::terminal::TerminalCommand;
use crate::{CommandFailure, DiscoveredService};

pub(super) async fn run(
    command: &TerminalCommand,
    terminal: &Path,
    directory: &str,
    ready: DiscoveredService,
) -> Result<u8, CommandFailure> {
    let health = ready
        .product_health()
        .map_err(|error| error.to_string())?
        .encoded()
        .map_err(|error| error.to_string())?;
    println!(
        "{}",
        crate::operator_text::opening_repository(directory, cfg!(windows))
    );
    let mut ui = super::selection::launch(command, terminal, directory, &health, &ready)
        .await?
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            format!(
                "terminal cannot start: {:?}; extract grapt-tui beside grapt",
                error.kind()
            )
        })?;
    tokio::select! {
        status = ui.wait() => {
            let status = status
                .map_err(|error| format!("terminal exit is unreadable: {:?}", error.kind()))?;
            Ok(if status.success() { 0 } else { 1 })
        }
        signal = tokio::signal::ctrl_c() => {
            super::finish_terminal(&mut ui).await?;
            signal.map_err(|error| format!("cancellation observer failed: {:?}", error.kind()))?;
            Ok(130)
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
