//! Local setup migration shared by the shell command and native import flow.
//!
//! Discovery and publication for external-agent migration.
//! Copyright 2025 OpenAI. Apache-2.0 (licenses/openai-codex/).
//! Modified for bounded reads, retained source text and exclusive creation.
use crate::command::import::{ImportCommand, ImportKind, ImportSource};
use std::path::{Path, PathBuf};

pub mod agents;
mod destination;
mod files;
mod graph;
mod hooks;
mod instructions;
mod metadata;
mod rules;
mod skills;

/// Preview by default. Existing files and skill directories always win.
pub fn files(command: ImportCommand) -> Result<Vec<String>, String> {
    let directory = match command.directory {
        Some(path) => PathBuf::from(path),
        None => std::env::current_dir().map_err(|_| "import working directory is unavailable")?,
    };
    let directory =
        std::fs::canonicalize(directory).map_err(|_| "import directory cannot be resolved")?;
    let home = home()?;
    let mut plan = files::Plan::default();
    match command.kind {
        ImportKind::Instructions => {
            instructions::discover(&directory, &home, command.source, &mut plan)?
        }
        ImportKind::Skills => skills::discover(&directory, &home, command.source, &mut plan)?,
        _ => return Err("this import kind requires the running service".into()),
    }
    plan.finish(command.apply, command.kind.label(), command.source.label())
}

fn home() -> Result<PathBuf, String> {
    let value = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .ok_or("import home directory is unavailable")?;
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err("import home directory must be absolute".into());
    }
    Ok(path)
}

fn display(path: &Path) -> String {
    path.to_string_lossy().escape_debug().to_string()
}

fn roots(directory: &Path, home: &Path, source: ImportSource) -> Vec<(PathBuf, PathBuf)> {
    let family = match source {
        ImportSource::Claude => ".claude",
        ImportSource::Codex => ".codex",
    };
    vec![
        (directory.join(family), directory.to_path_buf()),
        (home.join(family), home.to_path_buf()),
    ]
}

/// Run one explicitly selected import kind. No report contains source contents.
pub async fn run(
    command: ImportCommand,
    limits: crate::CliLimits,
) -> Result<Vec<String>, (crate::ExitClass, String)> {
    if command.kind == ImportKind::Hooks && command.source != ImportSource::Claude {
        return Err((crate::ExitClass::Usage, "hooks supports Claude only".into()));
    }
    match command.kind {
        ImportKind::Mcp => crate::executor::import::report(command, limits).await,
        ImportKind::Hooks => hooks::run(command, limits).await,
        _ => tokio::task::spawn_blocking(move || files(command))
            .await
            .map_err(|_| {
                (
                    crate::ExitClass::Output,
                    "import file reader could not finish".into(),
                )
            })?
            .map_err(|reason| (crate::ExitClass::Usage, reason)),
    }
}
