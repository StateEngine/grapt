// Copyright 2025 OpenAI
// SPDX-License-Identifier: Apache-2.0
// Modified by Grapt: explicit paths, backlink checks, FF-only merge, no force cleanup.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::git::{git_mutation, git_output, git_path, path_from_bytes};
use crate::WorktreeError;

/// Fast-forward the selected source repository to a linked checkout's commits.
///
/// Both working trees must be clean, including untracked files. Ignored files
/// may remain but cannot be overwritten. Divergence and local edits are refused;
/// no stash, merge commit, hooks, filters, editor, or signature helper is used.
/// Assume-unchanged/skip-worktree index entries are refused even if unmodified.
/// Registration files are read through Unix descriptors or Windows pinned handles.
pub async fn merge_back(source: &Path, checkout: &Path) -> Result<(), WorktreeError> {
    let (source, checkout) = registered(source, checkout)
        .await
        .map_err(WorktreeError::NotStarted)?;
    clean(&source, false)
        .await
        .map_err(WorktreeError::NotStarted)?;
    clean(&checkout, false)
        .await
        .map_err(WorktreeError::NotStarted)?;
    let source_head = head(&source).await.map_err(WorktreeError::NotStarted)?;
    let checkout_head = head(&checkout).await.map_err(WorktreeError::NotStarted)?;
    ancestor(&source, &source_head, &checkout_head)
        .await
        .map_err(|e| {
            WorktreeError::NotStarted(format!(
                "cannot fast-forward source; rebase committed checkout work onto source first: {e}"
            ))
        })?;
    git_mutation(
        &source,
        &[
            "--work-tree=.",
            "merge",
            "--ff-only",
            "--no-squash",
            "--no-edit",
            "--no-stat",
            "--no-gpg-sign",
            "--no-verify-signatures",
            "--no-autostash",
            "--no-overwrite-ignore",
            "--",
            &checkout_head,
        ],
    )
    .await
    .map_err(|e| {
        e.with_context("fast-forward refused; source edits were not stashed or discarded")
    })?;
    Ok(())
}

/// Remove a registered linked checkout only after its committed work is merged.
///
/// Never removes the source, main repository, or process's current checkout.
/// Dirty, untracked, ignored, and unmerged file content is refused. Empty ignored
/// or untracked directories are scaffold removed by Git, without force. No
/// separate filesystem pruning or cleanup on failure occurs.
/// Assume-unchanged/skip-worktree index entries are refused without changing them.
/// Registration files are read through Unix descriptors or Windows pinned handles.
/// Cleanup requires Linux, macOS, or Windows descendant mount/link safeguards.
pub async fn remove(source: &Path, checkout: &Path) -> Result<(), WorktreeError> {
    crate::guarded::cleanup_supported().map_err(WorktreeError::NotStarted)?;
    let (source, checkout) = registered(source, checkout)
        .await
        .map_err(WorktreeError::NotStarted)?;
    let current = std::env::current_dir()
        .and_then(std::fs::canonicalize)
        .map_err(|e| {
            WorktreeError::NotStarted(format!(
                "cannot verify current directory before removal: {e}"
            ))
        })?;
    if current.starts_with(&checkout) {
        return Err(WorktreeError::NotStarted(
            "switch out of the current checkout before removing it".into(),
        ));
    }
    // Git status can omit nested .git files and special nodes. Inspect actual
    // entries against the bounded index, with no handles surviving an await.
    let tracked = crate::git::tracked_paths(&checkout)
        .await
        .map_err(WorktreeError::NotStarted)?;
    let retained =
        crate::guarded::retained_content(&checkout, &tracked).map_err(WorktreeError::NotStarted)?;
    if let Some(path) = retained {
        // A special node replacing a tracked file is an uncommitted change,
        // even if a no-index ignore query would match that tracked pathname.
        let ignored = if tracked.contains(&path) {
            false
        } else {
            crate::git::path_is_ignored(&checkout, &path)
                .await
                .map_err(WorktreeError::NotStarted)?
        };
        return Err(WorktreeError::NotStarted(crate::status::retained_path(
            &path, ignored,
        )));
    }
    clean(&checkout, true)
        .await
        .map_err(WorktreeError::NotStarted)?;
    let source_head = head(&source).await.map_err(WorktreeError::NotStarted)?;
    let checkout_head = head(&checkout).await.map_err(WorktreeError::NotStarted)?;
    ancestor(&source, &checkout_head, &source_head)
        .await
        .map_err(|e| {
            WorktreeError::NotStarted(format!(
                "checkout has commits not merged into source; merge them before removal: {e}"
            ))
        })?;
    let argument = crate::path::git_argument(&checkout).map_err(WorktreeError::NotStarted)?;
    git_mutation(
        &source,
        &[
            OsStr::new("--work-tree=."),
            OsStr::new("worktree"),
            OsStr::new("remove"),
            OsStr::new("--"),
            argument.as_os_str(),
        ],
    )
    .await
    .map_err(|e| e.with_context("worktree removal refused; checkout was not force-removed"))?;
    Ok(())
}

pub(super) async fn repository_root(source: &Path) -> Result<PathBuf, String> {
    let source = canonical(source)?;
    if !source.is_dir() {
        return Err("source working directory must be a directory".into());
    }
    let mut fence = None;
    for ancestor in source.ancestors() {
        match std::fs::symlink_metadata(ancestor.join(".git")) {
            Ok(_) => {
                fence = Some(ancestor);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "cannot inspect selected repository boundary: {error}"
                ))
            }
        }
    }
    let fence = fence.ok_or("source has no enclosing .git repository boundary")?;
    let root = canonical(&git_path(fence, &["rev-parse", "--show-toplevel"]).await?)?;
    if root != fence {
        return Err("Git worktree differs from the selected repository root; remove the core.worktree redirect before retrying".into());
    }
    Ok(root)
}

async fn registered(source: &Path, checkout: &Path) -> Result<(PathBuf, PathBuf), String> {
    let source = repository_root(source).await?;
    let checkout = canonical(checkout)?;
    if source == checkout || source.starts_with(&checkout) {
        return Err(
            "refusing the source/current repository; select a different linked checkout".into(),
        );
    }
    // Inspect a bounded, actually opened regular .git file before invoking Git
    // in the candidate (main repositories and link/junction aliases are refused).
    let front = crate::guarded::read(&checkout, Path::new(".git"), 4096)
        .map_err(|e| format!("checkout is not a safe linked worktree: {e}"))?;
    let common = resolve(&source, "--git-common-dir").await?;
    let checkout_root = canonical(
        &git_path(
            &checkout,
            &["--work-tree=.", "rev-parse", "--show-toplevel"],
        )
        .await?,
    )?;
    if checkout_root != checkout || resolve(&checkout, "--git-common-dir").await? != common {
        return Err("checkout is not a root in the selected source repository".into());
    }
    let admin = resolve(&checkout, "--git-dir").await?;
    let worktrees = canonical(&common.join("worktrees"))?;
    if admin == common || admin.parent() != Some(worktrees.as_path()) {
        return Err(
            "checkout has no linked-worktree administration entry in this repository".into(),
        );
    }
    let front = trim_line(&front)
        .strip_prefix(b"gitdir: ")
        .ok_or("checkout .git file has no gitdir pointer")?;
    if canonical(&checkout.join(path_from_bytes(front)?))? != admin {
        return Err("checkout .git pointer does not match its administration directory".into());
    }
    let backlink = crate::guarded::read(&admin, Path::new("gitdir"), 4096)?;
    let backlink = path_from_bytes(trim_line(&backlink))?;
    if canonical(&admin.join(backlink))? != canonical(&checkout.join(".git"))? {
        return Err("worktree registration backlink does not name the selected checkout".into());
    }
    let entries = git_output(
        &source,
        &["--work-tree=.", "worktree", "list", "--porcelain", "-z"],
        false,
    )
    .await?;
    let mut found = false;
    for field in entries.split(|byte| *byte == 0) {
        if let Some(path) = field.strip_prefix(b"worktree ") {
            if canonical(&path_from_bytes(path)?).is_ok_and(|path| path == checkout) {
                found = true;
            }
        }
    }
    if !found {
        return Err(
            "checkout is not registered with this repository; repair registration before retrying"
                .into(),
        );
    }
    Ok((source, checkout))
}

async fn clean(root: &Path, ignored: bool) -> Result<(), String> {
    if crate::git::hidden_index_entries(root).await? {
        return Err(format!(
            "{} has assume-unchanged or skip-worktree index entries that can hide local edits; clear those flags and inspect changes before retrying",
            root.display()
        ));
    }
    let mut args = vec![
        "--work-tree=.",
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--ignore-submodules=none",
    ];
    if ignored {
        // With -uall, traditional ignored reporting names retained files but
        // not empty directory scaffold. Git alone owns any eventual pruning.
        args.push("--ignored=traditional");
    }
    let output = git_output(root, &args, true).await?;
    crate::status::ensure_clean(&output).map_err(|error| format!("{}: {error}", root.display()))
}

async fn head(root: &Path) -> Result<String, String> {
    let bytes = git_output(
        root,
        &["rev-parse", "--verify", "--end-of-options", "HEAD^{commit}"],
        false,
    )
    .await?;
    let head = std::str::from_utf8(trim_line(&bytes)).map_err(|_| "Git HEAD is not UTF-8")?;
    if !matches!(head.len(), 40 | 64) || !head.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Git returned an invalid commit id".into());
    }
    Ok(head.to_owned())
}

async fn ancestor(root: &Path, older: &str, newer: &str) -> Result<(), String> {
    git_output(root, &["merge-base", "--is-ancestor", older, newer], false).await?;
    Ok(())
}

async fn resolve(root: &Path, argument: &str) -> Result<PathBuf, String> {
    canonical(&root.join(git_path(root, &["rev-parse", "--path-format=absolute", argument]).await?))
}

fn canonical(path: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(path).map_err(|e| format!("cannot resolve {}: {e}", path.display()))
}

fn trim_line(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    #[cfg(windows)]
    let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    bytes
}
