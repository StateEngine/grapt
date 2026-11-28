//! Explicit worktree effects at the authenticated, canonical location boundary.
use std::path::{Component, Path, PathBuf};

use grapt_api::u3a::{
    ApplicationFuture, BoundedString, InvalidRequestErrorDto, LocationInfoDto,
    ProjectCurrentLocationQueryDto, WorkspaceWorktreeDataDto, WorkspaceWorktreeEffectDto,
    WorkspaceWorktreeRequestDto, WorkspaceWorktreeResponseDto,
};
use grapt_schema::Required;
use serde_json::json;

use super::execution::WorkspaceIdleReservation;
use super::support::{bounded_message, decode, invalid, static_dto};
use super::{locations, ServerApplication};

pub(crate) fn response(
    application: ServerApplication,
    query: ProjectCurrentLocationQueryDto,
    request: WorkspaceWorktreeRequestDto,
) -> ApplicationFuture<WorkspaceWorktreeResponseDto, InvalidRequestErrorDto> {
    Box::pin(async move {
        // A mutating command must never inherit the service's default directory.
        if query
            .location()
            .as_value()
            .and_then(|location| location.directory().as_value())
            .is_none()
        {
            return Err(invalid(
                "Worktree operations require an explicit source directory",
            ));
        }
        let location = locations::resolve_for(&application, &query)
            .await
            .map_err(invalid)?;
        let source = std::fs::canonicalize(&location.directory).map_err(invalid)?;
        // Validate the source response before any operation can change files.
        let source_location: LocationInfoDto = decode(location.wire()).map_err(invalid)?;
        let source_directory = BoundedString::try_new(location.directory).map_err(invalid)?;
        let (directory, effect, message) = match operate(&application, &source, request).await {
            Ok((Some(path), message)) => {
                match path.to_str().and_then(|path| BoundedString::try_new(path).ok()) {
                    Some(directory) => (directory, WorkspaceWorktreeEffectDto::Completed, message.to_owned()),
                    None => (source_directory, WorkspaceWorktreeEffectDto::MayHaveChanged,
                        "Worktree was created but its directory cannot be represented; inspect the destination before retrying.".to_owned()),
                }
            }
            Ok((None, message)) => (
                source_directory,
                WorkspaceWorktreeEffectDto::Completed,
                message.to_owned(),
            ),
            Err(error) => (source_directory, error.effect, error.message),
        };
        Ok(reply(source_location, directory, effect, message))
    })
}

// None selects the already validated source cwd; only create returns a new cwd.
async fn operate(
    application: &ServerApplication,
    source: &Path,
    request: WorkspaceWorktreeRequestDto,
) -> Result<(Option<PathBuf>, &'static str), OperationFailure> {
    match request {
        WorkspaceWorktreeRequestDto::Create {
            destination,
            setup_files,
        } => {
            if setup_files.len() > 32 {
                return Err(OperationFailure::not_started(
                    "Worktree setup exceeds 32 files; select fewer explicit setup files",
                ));
            }
            let destination =
                relative_to(source, destination.as_str()).map_err(OperationFailure::not_started)?;
            let setup_files: Vec<PathBuf> = setup_files
                .iter()
                .map(|file| PathBuf::from(file.as_str()))
                .collect();
            let directory =
                grapt_worktree::create_with_setup(source, &destination, &setup_files).await?;
            Ok((
                Some(directory),
                "Worktree created; source files are unchanged.",
            ))
        }
        WorkspaceWorktreeRequestDto::Merge { directory } => {
            let checkout =
                checkout_path(source, directory.as_str()).map_err(OperationFailure::not_started)?;
            let _idle = reserve_affected(application, source, &checkout)
                .map_err(OperationFailure::not_started)?;
            grapt_worktree::merge_back(source, &checkout).await?;
            Ok((
                None,
                "Committed worktree changes fast-forwarded into the source.",
            ))
        }
        WorkspaceWorktreeRequestDto::Remove { directory } => {
            let checkout =
                checkout_path(source, directory.as_str()).map_err(OperationFailure::not_started)?;
            let _idle = reserve_affected(application, source, &checkout)
                .map_err(OperationFailure::not_started)?;
            grapt_worktree::remove(source, &checkout).await?;
            Ok((None, "Clean, merged worktree removed without force."))
        }
    }
}

struct OperationFailure {
    effect: WorkspaceWorktreeEffectDto,
    message: String,
}

impl OperationFailure {
    fn not_started(message: impl ToString) -> Self {
        Self {
            effect: WorkspaceWorktreeEffectDto::NotStarted,
            message: message.to_string(),
        }
    }
}

impl From<grapt_worktree::WorktreeError> for OperationFailure {
    fn from(error: grapt_worktree::WorktreeError) -> Self {
        let effect = match error.effect() {
            grapt_worktree::WorktreeEffect::NotStarted => WorkspaceWorktreeEffectDto::NotStarted,
            grapt_worktree::WorktreeEffect::MayHaveChanged => {
                WorkspaceWorktreeEffectDto::MayHaveChanged
            }
        };
        Self {
            effect,
            message: error.message().to_owned(),
        }
    }
}

fn reply(
    location: LocationInfoDto,
    directory: BoundedString<4096>,
    effect: WorkspaceWorktreeEffectDto,
    message: impl ToString,
) -> WorkspaceWorktreeResponseDto {
    // Source and directory are already typed; bound arbitrary core diagnostics so
    // response encoding cannot turn an effect-bearing outcome into a generic 400.
    let original = message.to_string();
    let mut message = bounded_message(&original);
    if message.len() < original.len() {
        message
            .push_str(" [Diagnostics truncated; inspect source and destination before retrying.]");
    }
    let message = static_dto(json!(message));
    let data = WorkspaceWorktreeDataDto::new(
        Required::new(directory),
        Required::new(message),
        Required::new(effect),
    );
    WorkspaceWorktreeResponseDto::new(Required::new(location), Required::new(data))
}

fn relative_to(source: &Path, raw: &str) -> Result<PathBuf, String> {
    if raw.is_empty() || raw.chars().any(char::is_control) {
        return Err("Worktree directory must be a nonempty path without control characters".into());
    }
    let path = Path::new(raw);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    // Windows rooted/drive-relative paths must not replace the selected root.
    if path
        .components()
        .any(|part| matches!(part, Component::Prefix(_) | Component::RootDir))
    {
        return Err("Worktree directory must be absolute or relative to the source".into());
    }
    Ok(source.join(path))
}

fn checkout_path(source: &Path, raw: &str) -> Result<PathBuf, String> {
    std::fs::canonicalize(relative_to(source, raw)?)
        .map_err(|error| format!("Worktree directory is unreadable: {error}"))
}

fn repository_root(directory: &Path) -> Result<PathBuf, String> {
    let root = locations::repository_root(directory)?
        .ok_or("Worktree operations require a Git repository")?;
    std::fs::canonicalize(root).map_err(|error| error.to_string())
}

fn reserve_affected(
    application: &ServerApplication,
    source: &Path,
    checkout: &Path,
) -> Result<Vec<WorkspaceIdleReservation>, String> {
    let mut roots = vec![repository_root(source)?, repository_root(checkout)?];
    roots.sort();
    roots.dedup();
    // One parent reservation already excludes every admitted nested workspace.
    if roots.len() == 2 && roots[1].starts_with(&roots[0]) {
        roots.pop();
    }
    roots
        .iter()
        .map(|root| {
            application
                .wake_slots
                .reserve_workspace_idle(root)
                .map_err(|error| {
                    format!("Worktree mutation requires idle source and checkout: {error}")
                })
        })
        .collect()
    // Only owned permits survive the caller's await. No registry/inbox guard
    // escapes here; failed second acquisition releases the first reservation.
}

#[cfg(test)]
#[path = "workspace_worktree_tests.rs"]
mod tests;
