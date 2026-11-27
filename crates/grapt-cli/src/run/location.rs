//! A new noninteractive run belongs to the caller's resolved working directory.

use grapt_api::u3a::DtoString;
use grapt_types::SessionId;

use super::{RunError, Runner};

pub(super) async fn create(runner: &Runner) -> Result<(SessionId, u32), RunError> {
    let directory = std::env::current_dir()
        .and_then(std::fs::canonicalize)
        .map_err(|error| {
            RunError::WorkingDirectory(format!(
                "cannot resolve the current directory: {:?}",
                error.kind()
            ))
        })?;
    let directory = match &runner.worktree {
        Some(destination) => grapt_worktree::create(&directory, destination)
            .await
            .map_err(RunError::WorkingDirectory)?,
        None => directory,
    };
    let directory = directory.to_str().ok_or_else(|| {
        RunError::WorkingDirectory("current directory must be a UTF-8 path".into())
    })?;
    let directory = DtoString::try_new(directory.to_owned()).map_err(|_| {
        RunError::WorkingDirectory("current directory exceeds its wire bound".into())
    })?;
    let created = runner
        .client
        .create_session_at_selected(directory, &runner.selection)
        .await?;
    Ok((created.data().get().id().get().as_inner().clone(), 1))
}
