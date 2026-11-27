//! Explicit CLI selection creates one Session before launching the terminal.

use crate::command::terminal::TerminalCommand;
use crate::{ApiClient, CliLimits, CommandFailure, DiscoveredService};
use grapt_api::u3a::DtoString;
use std::path::Path;
use tokio::process::Command;

pub(super) async fn launch(
    command: &TerminalCommand,
    terminal: &Path,
    directory: &str,
    health: &str,
    ready: &DiscoveredService,
) -> Result<Command, CommandFailure> {
    let session = if command.selection.is_empty() {
        None
    } else {
        let client = ApiClient::from_discovered(ready, CliLimits::production())?;
        let directory = DtoString::try_new(directory.to_owned())
            .map_err(|_| "repository directory exceeds its wire bound")?;
        let created = client
            .create_session_at_selected(directory, &command.selection)
            .await
            .map_err(|error| {
                CommandFailure::from(error).context("cannot start selected session")
            })?;
        let session = created.data().get().id().get().as_inner().clone();
        if let Some(effort) = command.reasoning_effort.as_deref() {
            client
                .select_reasoning_effort(&session, effort)
                .await
                .map_err(|error| {
                    CommandFailure::from(error).context("cannot select reasoning effort")
                })?;
        }
        Some(session)
    };
    let effort = if session.is_some() {
        None
    } else {
        command.reasoning_effort.as_deref()
    };
    Ok(super::terminal_command(
        terminal,
        &command.profile,
        directory,
        health,
        command.preset,
        effort,
        session.as_ref(),
    ))
}
