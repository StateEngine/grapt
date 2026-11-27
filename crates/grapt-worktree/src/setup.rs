// Copyright 2025 OpenAI
// SPDX-License-Identifier: Apache-2.0
// Modified by Grapt: explicit bounded setup allowlist, no automatic inventory.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use crate::{git::git_output, guarded, lifecycle::repository_root, WorktreeError};

const MAX_FILES: usize = 32;
const MAX_FILE: usize = 1024 * 1024;
const MAX_TOTAL: usize = 8 * MAX_FILE;

/// Create a detached checkout and copy an explicit repository-relative allowlist.
///
/// Only untracked/ignored regular files without symlink/junction/reparse traversal
/// are accepted. Windows hard-linked sources become independent file snapshots.
/// Copies are
/// private snapshots (at most 32 files, 1 MiB each, 8 MiB total), never overwrite,
/// and are rooted at the checkout root, not the returned nested working directory.
/// Failed creation/copying preserves any incomplete checkout for inspection.
/// Copying uses Unix descriptor-relative access or Windows pinned handles.
/// Snapshot/preflight failures are NotStarted; any failure after creation starts
/// is MayHaveChanged, including a failed setup write into a preserved checkout.
pub async fn create_with_setup(
    source: &Path,
    destination: &Path,
    setup_files: &[PathBuf],
) -> Result<PathBuf, WorktreeError> {
    if setup_files.is_empty() {
        return crate::create_checked(source, destination).await;
    }
    if setup_files.len() > MAX_FILES {
        return Err(WorktreeError::NotStarted(
            "setup allowlist exceeds 32 files; select fewer explicit files".into(),
        ));
    }
    let root = repository_root(source)
        .await
        .map_err(WorktreeError::NotStarted)?;
    let mut unique = BTreeSet::new();
    for relative in setup_files {
        validate(relative).map_err(WorktreeError::NotStarted)?;
        if !unique.insert(relative) {
            return Err(WorktreeError::NotStarted(format!(
                "duplicate setup path: {}",
                relative.display()
            )));
        }
        let tracked = git_output(
            &root,
            &[
                OsStr::new("--work-tree=."),
                OsStr::new("--literal-pathspecs"),
                OsStr::new("ls-files"),
                OsStr::new("--cached"),
                OsStr::new("-z"),
                OsStr::new("--"),
                relative.as_os_str(),
            ],
            false,
        )
        .await
        .map_err(WorktreeError::NotStarted)?;
        if !tracked.is_empty() {
            return Err(WorktreeError::NotStarted(format!(
                "setup path {} is tracked; use only untracked/ignored files",
                relative.display()
            )));
        }
    }
    // All Git awaits finish before acquiring any filesystem guards.
    let mut files = Vec::new();
    let mut total = 0;
    for relative in setup_files {
        let bytes = guarded::read(&root, relative, MAX_FILE).map_err(|e| {
            WorktreeError::NotStarted(format!("cannot copy setup {}: {e}", relative.display()))
        })?;
        total += bytes.len();
        if total > MAX_TOTAL {
            return Err(WorktreeError::NotStarted(
                "setup allowlist exceeds 8 MiB total; select smaller files".into(),
            ));
        }
        files.push((relative.clone(), bytes));
    }
    let destination =
        crate::absolute_destination(destination).map_err(WorktreeError::NotStarted)?;
    let cwd = crate::create_checked(source, &destination).await?;
    guarded::write(&destination, &files).map_err(|e| {
        WorktreeError::MayHaveChanged(format!(
            "{e}; incomplete checkout preserved at {}",
            destination.display()
        ))
    })?;
    Ok(cwd)
}

fn validate(relative: &Path) -> Result<(), String> {
    if relative.as_os_str().is_empty()
        || relative.to_str().is_none_or(|path| path.len() > 4096)
        || relative.components().count() > 128
    {
        return Err(
            "setup path must be nonempty UTF-8, at most 4096 bytes and 128 components".into(),
        );
    }
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err("setup paths must be relative without traversal".into());
        };
        let name = name.to_str().ok_or("setup path is not UTF-8")?;
        let stem = name
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || stem
                .strip_prefix("COM")
                .or_else(|| stem.strip_prefix("LPT"))
                .is_some_and(|suffix| {
                    matches!(
                        suffix,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
                });
        if name.eq_ignore_ascii_case(".git")
            || name.to_ascii_lowercase().starts_with("git~")
            || name.ends_with(['.', ' '])
            || device
            || name
                .chars()
                .any(|ch| ch.is_ascii_control() || "<>:\"|?*\\/".contains(ch))
        {
            return Err(
                "setup path contains .git, a reserved name, or an aliased component".into(),
            );
        }
    }
    Ok(())
}
