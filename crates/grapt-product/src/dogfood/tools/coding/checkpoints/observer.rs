//! Independent read-only Git observation; capture and restore never run Git.
use grapt_checkpoint::{VcsObserver, WorkspaceObservation};
use grapt_types::{
    BranchName, BranchState, GitCommitId, PathIdentity, VcsBaseline, VcsUnavailableReason,
    WorktreeIdentity,
};
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(super) struct Observer {
    pub root: PathBuf,
}

fn text(path: &Path, bound: u64) -> Result<Option<String>, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if file.metadata().map_err(|error| error.to_string())?.len() > bound {
        return Err("Git metadata exceeds its read bound".into());
    }
    let mut bytes = Vec::new();
    file.take(bound + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > bound {
        return Err("Git metadata grew beyond its read bound".into());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn identity(path: &Path) -> Result<PathIdentity, String> {
    let path = std::fs::canonicalize(path).map_err(|error| error.to_string())?;
    let value = path
        .to_str()
        .filter(|value| value.len() <= 4096)
        .ok_or("Git metadata path is invalid")?;
    Ok(PathIdentity::canonical(value))
}

fn unavailable() -> VcsBaseline {
    VcsBaseline::Unavailable {
        reason: VcsUnavailableReason::MetadataUnreadable,
    }
}

impl Observer {
    fn inspect(&self) -> Result<(WorktreeIdentity, VcsBaseline), String> {
        let root = identity(&self.root)?;
        let Some(ancestor) = crate::dogfood::locations::repository_root_with_limit(&self.root, 64)?
        else {
            return Ok((WorktreeIdentity::Plain { root }, VcsBaseline::NotVersioned));
        };
        let candidate = ancestor.join(".git");
        let git = match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) if super::manifest::linked(&metadata) => {
                return Err("Git metadata is linked".into());
            }
            Ok(metadata) if metadata.is_dir() => candidate,
            Ok(metadata) if metadata.is_file() => {
                let pointer = text(&candidate, 4096)?.ok_or("Git pointer disappeared")?;
                let directory = pointer
                    .trim()
                    .strip_prefix("gitdir: ")
                    .ok_or("Git pointer is malformed")?;
                std::fs::canonicalize(ancestor.join(directory))
                    .map_err(|error| error.to_string())?
            }
            Ok(_) => return Err("Git metadata has an unsupported kind".into()),
            Err(error) => return Err(error.to_string()),
        };
        let common = match text(&git.join("commondir"), 4096)? {
            Some(pointer) => std::fs::canonicalize(git.join(pointer.trim()))
                .map_err(|error| error.to_string())?,
            None => git.clone(),
        };
        let worktree = WorktreeIdentity::Git {
            root,
            common_dir: identity(&common)?,
            git_dir: identity(&git)?,
        };
        let head = text(&git.join("HEAD"), 4096)?.ok_or("Git HEAD is absent")?;
        let head = head.trim();
        let Some(reference) = head.strip_prefix("ref: ") else {
            return Ok((
                worktree,
                VcsBaseline::Git {
                    head: GitCommitId::new(head).ok_or("Git HEAD is malformed")?,
                    branch: BranchState::Detached,
                },
            ));
        };
        let branch_text = reference
            .strip_prefix("refs/heads/")
            .ok_or("Git HEAD is not a branch")?;
        if Path::new(branch_text)
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err("Git branch reference escapes its namespace".into());
        }
        let branch = BranchName::new(branch_text).ok_or("Git branch name is malformed")?;
        let commit = match text(&common.join(reference), 4096)? {
            Some(commit) => Some(commit.trim().to_owned()),
            None => {
                let mut matched = None;
                if let Some(packed) = text(&common.join("packed-refs"), 4 * 1024 * 1024)? {
                    for line in packed.lines() {
                        if line.starts_with(['#', '^']) || line.is_empty() {
                            continue;
                        }
                        let (commit, name) = line
                            .split_once(' ')
                            .ok_or("packed Git reference is malformed")?;
                        if name == reference {
                            if matched.is_some() {
                                return Err("packed Git reference is duplicated".into());
                            }
                            matched = Some(commit.to_owned());
                        }
                    }
                }
                matched
            }
        };
        let baseline = match commit {
            Some(commit) => VcsBaseline::Git {
                head: GitCommitId::new(commit).ok_or("Git commit is malformed")?,
                branch: BranchState::Symbolic(branch),
            },
            None => VcsBaseline::GitUnborn { branch },
        };
        Ok((worktree, baseline))
    }
}

impl VcsObserver for Observer {
    fn observe(&mut self, _root: &PathIdentity) -> WorkspaceObservation {
        match self.inspect() {
            Ok((worktree, baseline)) => WorkspaceObservation::new(worktree, baseline),
            Err(_) => WorkspaceObservation::new(
                WorktreeIdentity::Plain {
                    root: PathIdentity::canonical(self.root.to_string_lossy()),
                },
                unavailable(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_observer_respects_the_repository_ceiling() -> Result<(), std::io::Error> {
        let root =
            std::env::temp_dir().join(format!("grapt-checkpoint-ceiling-{}", std::process::id()));
        std::fs::create_dir(&root)?;
        for directory in [root.clone(), root.join("plain/nested")] {
            std::fs::create_dir_all(directory.join(".git"))?;
            std::fs::write(directory.join(".git/HEAD"), "ref: refs/heads/main\n")?;
        }
        std::fs::create_dir_all(root.join("broken/.git"))?;
        let result = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "dogfood::tools::coding::checkpoints::observer::tests::ceiling_child",
                "--ignored",
                "--nocapture",
            ])
            .env("GRAPT_CHECKPOINT_CEILING_FIXTURE", &root)
            .env("GIT_CEILING_DIRECTORIES", &root)
            .status();
        let cleanup = std::fs::remove_dir_all(&root);
        let status = result?;
        cleanup?;
        assert!(
            status.success(),
            "checkpoint ceiling child failed: {status}"
        );
        Ok(())
    }

    #[test]
    #[ignore = "isolated child of checkpoint_observer_respects_the_repository_ceiling"]
    fn ceiling_child() -> Result<(), String> {
        let root = PathBuf::from(
            std::env::var_os("GRAPT_CHECKPOINT_CEILING_FIXTURE")
                .ok_or("missing ceiling fixture")?,
        );
        let (worktree, baseline) = Observer {
            root: root.join("plain"),
        }
        .inspect()?;
        assert!(matches!(worktree, WorktreeIdentity::Plain { .. }));
        assert_eq!(baseline, VcsBaseline::NotVersioned);
        assert!(Observer {
            root: root.join("broken")
        }
        .inspect()
        .is_err());
        for directory in [root.clone(), root.join("plain/nested")] {
            let (worktree, baseline) = Observer { root: directory }.inspect()?;
            assert!(matches!(worktree, WorktreeIdentity::Git { .. }));
            assert!(matches!(baseline, VcsBaseline::GitUnborn { .. }));
        }
        Ok(())
    }
}
