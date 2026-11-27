//! Git baseline observation and fixture repository control for checkpoint
//! acceptance.
//!
//! The observer reads Git metadata files and never runs Git, so observing a
//! workspace can neither move HEAD nor stage anything. Only explicit corpus
//! actions change the fixture repository, through the Git CLI with a fixed
//! identity, and never as a side effect of capture, plan, apply or recovery.

use super::FixtureError;
use grapt_types::{BranchName, BranchState, GitCommitId, VcsBaseline, VcsUnavailableReason};
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

/// Bounded read of one Git metadata file.
fn metadata(path: &Path) -> Option<String> {
    const MAX_METADATA_BYTES: u64 = 4096;
    let size = fs::metadata(path).ok()?.len();
    if size > MAX_METADATA_BYTES {
        return None;
    }
    fs::read_to_string(path).ok()
}

/// The baseline of `workspace` as its Git metadata states it.
pub(super) fn observe(workspace: &Path) -> VcsBaseline {
    let git = workspace.join(".git");
    if !git.exists() {
        return VcsBaseline::NotVersioned;
    }
    let unavailable = || VcsBaseline::Unavailable {
        reason: VcsUnavailableReason::MetadataUnreadable,
    };
    let Some(head) = metadata(&git.join("HEAD")) else {
        return unavailable();
    };
    let head = head.trim();
    let Some(reference) = head.strip_prefix("ref: ") else {
        return match GitCommitId::new(head) {
            Some(head) => VcsBaseline::Git {
                head,
                branch: BranchState::Detached,
            },
            None => unavailable(),
        };
    };
    let Some(branch) = reference
        .strip_prefix("refs/heads/")
        .and_then(BranchName::new)
    else {
        return unavailable();
    };
    match metadata(&git.join(reference)) {
        Some(commit) => match GitCommitId::new(commit.trim()) {
            Some(head) => VcsBaseline::Git {
                head,
                branch: BranchState::Symbolic(branch),
            },
            None => unavailable(),
        },
        // A loose ref may legitimately be packed; a packed-refs file this
        // fixture does not parse is unreadable, never an unborn branch.
        None if git.join("packed-refs").exists() => unavailable(),
        None => VcsBaseline::GitUnborn { branch },
    }
}

/// A stable textual form of a baseline for acceptance receipts.
pub(super) fn describe(baseline: &VcsBaseline) -> String {
    match baseline {
        VcsBaseline::Git { head, .. } => format!("git:{head}"),
        VcsBaseline::GitUnborn { branch } => format!("unborn:{}", branch.as_str()),
        VcsBaseline::NotVersioned => "not-versioned".to_owned(),
        VcsBaseline::Unavailable { .. } => "unavailable".to_owned(),
    }
}

fn git(workspace: &Path, arguments: &[&str]) -> Result<(), FixtureError> {
    let status = Command::new("git")
        .env(
            "GIT_CEILING_DIRECTORIES",
            workspace.parent().unwrap_or(workspace),
        )
        .arg("-C")
        .arg(workspace)
        .args([
            "-c",
            "user.name=checkpoint-fixture",
            "-c",
            "user.email=checkpoint-fixture@invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| FixtureError::GitUnavailable)?;
    if status.success() {
        Ok(())
    } else {
        Err(FixtureError::GitUnavailable)
    }
}

pub(super) fn init(workspace: &Path) -> Result<(), FixtureError> {
    git(workspace, &["init", "--quiet", "--initial-branch=main"])
}

/// Commits every workspace file and returns the new HEAD commit.
pub(super) fn commit(workspace: &Path, message: &str) -> Result<GitCommitId, FixtureError> {
    git(workspace, &["add", "--all"])?;
    git(
        workspace,
        &["commit", "--quiet", "--allow-empty", "-m", message],
    )?;
    observe(workspace)
        .head()
        .cloned()
        .ok_or(FixtureError::GitUnavailable)
}
