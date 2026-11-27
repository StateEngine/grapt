// Copyright 2025 OpenAI
// SPDX-License-Identifier: Apache-2.0
// Modified by Grapt: explicit destinations, bounded async Git and guarded lifecycle.

//! Bounded detached worktree creation, explicit setup copying, and safe lifecycle.

mod error;
mod git;
mod guarded;
mod lifecycle;
mod path;
mod setup;
mod status;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use git::{git_output, git_path};

pub use error::{WorktreeEffect, WorktreeError};
pub use lifecycle::{merge_back, remove};
pub use setup::create_with_setup;

/// Create a new detached checkout at the source repository's current HEAD.
///
/// The returned directory preserves a nested source cwd. The destination must
/// not exist and its parent must exist. Dirty source files remain untouched.
/// Commands have bounded output and deadlines; failed checkouts are preserved
/// for inspection rather than deleted.
pub async fn create(source: &Path, destination: &Path) -> Result<PathBuf, String> {
    create_checked(source, destination)
        .await
        .map_err(|error| error.to_string())
}

/// Create a detached checkout with an explicit failure effect.
///
/// Preflight failures are [`WorktreeEffect::NotStarted`]. Once worktree add is
/// attempted, all failures are [`WorktreeEffect::MayHaveChanged`], including
/// post-populate cwd resolution. Incomplete checkouts are never cleaned up.
pub async fn create_checked(source: &Path, destination: &Path) -> Result<PathBuf, WorktreeError> {
    let source =
        std::fs::canonicalize(source).map_err(|e| WorktreeError::NotStarted(e.to_string()))?;
    let repository = lifecycle::repository_root(&source)
        .await
        .map_err(WorktreeError::NotStarted)?;
    let relative = source.strip_prefix(&repository).map_err(|_| {
        WorktreeError::NotStarted("working directory is outside the repository".into())
    })?;
    let destination = absolute_destination(destination).map_err(WorktreeError::NotStarted)?;
    let cwd = destination.join(relative);
    if cwd.to_str().is_none_or(|path| path.len() > 4_096) {
        return Err(WorktreeError::NotStarted(
            "worktree working directory must be a UTF-8 path of at most 4096 bytes".into(),
        ));
    }
    let head = git_output(
        &source,
        &["rev-parse", "--verify", "--end-of-options", "HEAD^{commit}"],
        false,
    )
    .await
    .map_err(WorktreeError::NotStarted)?;
    let head = std::str::from_utf8(&head)
        .map_err(|_| WorktreeError::NotStarted("Git HEAD is not UTF-8".into()))?
        .trim();
    if !matches!(head.len(), 40 | 64) || !head.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(WorktreeError::NotStarted(
            "Git returned an invalid commit id".into(),
        ));
    }
    let argument = path::git_argument(&destination).map_err(WorktreeError::NotStarted)?;
    // All destination/head validation is complete. populate begins by attempting
    // worktree add; even spawn/output/timeout failure may have left effects.
    let result = populate(&repository, argument.as_ref(), head).await;
    if let Err(error) = result {
        // Never force-remove a checkout after a failed or timed-out command.
        return Err(WorktreeError::MayHaveChanged(format!(
            "{error}; any incomplete worktree is preserved at {}",
            destination.display()
        )));
    }
    let resolved = std::fs::canonicalize(&cwd).map_err(|error| {
        WorktreeError::MayHaveChanged(format!(
            "worktree does not contain working directory {}: {error}; checkout preserved at {}",
            relative.display(),
            destination.display()
        ))
    })?;
    let root = std::fs::canonicalize(&destination)
        .map_err(|error| WorktreeError::MayHaveChanged(error.to_string()))?;
    if !resolved.is_dir() || !resolved.starts_with(&root) {
        return Err(WorktreeError::MayHaveChanged(format!(
            "worktree working directory escapes its checkout; checkout preserved at {}",
            destination.display()
        )));
    }
    // Session placement normalizes separators. Return Git's ordinary spelling
    // so that conversion cannot turn a verbatim prefix into a different path.
    #[cfg(windows)]
    let resolved = path::git_argument(&resolved)
        .map_err(WorktreeError::MayHaveChanged)?
        .into_owned();
    Ok(resolved)
}

async fn populate(repository: &Path, destination: &Path, head: &str) -> Result<(), String> {
    git_output(
        repository,
        &[
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--detach"),
            OsStr::new("--no-checkout"),
            OsStr::new("--"),
            destination.as_os_str(),
            OsStr::new(head),
        ],
        false,
    )
    .await?;
    // Pin the destination even if the source has a configured core.worktree.
    let config = git_path(
        destination,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "config.worktree",
        ],
    )
    .await?;
    git_output(
        destination,
        &[
            OsStr::new("config"),
            OsStr::new("--file"),
            config.as_os_str(),
            OsStr::new("core.worktree"),
            destination.as_os_str(),
        ],
        false,
    )
    .await?;
    git_output(
        destination,
        &[
            "--work-tree=.",
            "reset",
            "--hard",
            "--no-recurse-submodules",
            head,
        ],
        true,
    )
    .await?;
    Ok(())
}

fn absolute_destination(destination: &Path) -> Result<PathBuf, String> {
    let destination: PathBuf = destination.components().collect();
    let name = match destination.components().next_back() {
        Some(std::path::Component::Normal(name)) => name,
        _ => return Err("worktree destination must name a new directory".into()),
    };
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = std::fs::canonicalize(parent)
        .map_err(|error| format!("cannot resolve worktree parent: {error}"))?;
    let destination = parent.join(name);
    match std::fs::symlink_metadata(&destination) {
        Ok(_) => Err("worktree destination already exists".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(destination),
        Err(error) => Err(format!("cannot inspect worktree destination: {error}")),
    }
}

#[cfg(test)]
mod tests;
