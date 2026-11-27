//! Retain the foundation's independent capture coordinates with patch receipts.
#[cfg(test)]
use grapt_checkpoint::WorkspaceObservation;
use grapt_types::{
    BranchName, BranchState, GitCommitId, PathIdentity, RecoveryAnchor, VcsBaseline,
    WorktreeIdentity,
};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Origin {
    pub worktree: WorktreeIdentity,
    pub baseline: VcsBaseline,
}

impl Origin {
    pub fn anchor(anchor: &RecoveryAnchor) -> Self {
        Self {
            worktree: anchor.worktree().clone(),
            baseline: anchor.baseline().clone(),
        }
    }
    #[cfg(test)]
    pub fn observation(observation: &WorkspaceObservation) -> Self {
        Self {
            worktree: observation.worktree().clone(),
            baseline: observation.baseline().clone(),
        }
    }
    pub fn encode(&self) -> Result<Value, String> {
        let worktree = match &self.worktree {
            WorktreeIdentity::Plain { root } => json!({"kind":"plain","root":canonical(root)?}),
            WorktreeIdentity::Git {
                root,
                common_dir,
                git_dir,
            } => {
                json!({"kind":"git","root":canonical(root)?,"commonDir":canonical(common_dir)?,"gitDir":canonical(git_dir)?})
            }
        };
        let baseline = match &self.baseline {
            VcsBaseline::NotVersioned => json!({"kind":"plain"}),
            VcsBaseline::Git { head, branch } => {
                let branch = match branch {
                    BranchState::Detached => json!({"kind":"detached"}),
                    BranchState::Symbolic(name) => json!({"kind":"symbolic","name":name.as_str()}),
                    _ => {
                        return Err(
                            "checkpoint origin does not pin an exact Git branch state".into()
                        )
                    }
                };
                json!({"kind":"git","head":head.as_str(),"branch":branch})
            }
            _ => return Err("checkpoint origin does not pin a readable baseline".into()),
        };
        Ok(json!({"worktree":worktree,"baseline":baseline}))
    }
    pub fn decode(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .filter(|value| value.len() == 2)
            .ok_or("invalid checkpoint origin")?;
        let worktree = object
            .get("worktree")
            .and_then(Value::as_object)
            .ok_or("missing worktree origin")?;
        let path = |key: &str| -> Result<PathIdentity, String> {
            let path = worktree
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| value.len() <= 4096)
                .ok_or("invalid checkpoint origin path")?;
            if !Path::new(path).is_absolute() {
                return Err("checkpoint origin path is not absolute".into());
            }
            Ok(PathIdentity::canonical(path))
        };
        let worktree = match worktree.get("kind").and_then(Value::as_str) {
            Some("plain") if worktree.len() == 2 => WorktreeIdentity::Plain {
                root: path("root")?,
            },
            Some("git") if worktree.len() == 4 => WorktreeIdentity::Git {
                root: path("root")?,
                common_dir: path("commonDir")?,
                git_dir: path("gitDir")?,
            },
            _ => return Err("invalid checkpoint worktree origin".into()),
        };
        let baseline = object
            .get("baseline")
            .and_then(Value::as_object)
            .ok_or("missing baseline origin")?;
        let baseline = match baseline.get("kind").and_then(Value::as_str) {
            Some("plain")
                if baseline.len() == 1 && matches!(worktree, WorktreeIdentity::Plain { .. }) =>
            {
                VcsBaseline::NotVersioned
            }
            Some("git")
                if baseline.len() == 3 && matches!(worktree, WorktreeIdentity::Git { .. }) =>
            {
                let head = baseline
                    .get("head")
                    .and_then(Value::as_str)
                    .and_then(GitCommitId::new)
                    .ok_or("invalid checkpoint Git commit")?;
                let branch = baseline
                    .get("branch")
                    .and_then(Value::as_object)
                    .ok_or("invalid checkpoint branch")?;
                let branch = match branch.get("kind").and_then(Value::as_str) {
                    Some("detached") if branch.len() == 1 => BranchState::Detached,
                    Some("symbolic") if branch.len() == 2 => BranchState::Symbolic(
                        branch
                            .get("name")
                            .and_then(Value::as_str)
                            .filter(|value| value.len() <= 4096)
                            .and_then(BranchName::new)
                            .ok_or("invalid checkpoint branch name")?,
                    ),
                    _ => return Err("invalid checkpoint branch origin".into()),
                };
                VcsBaseline::Git { head, branch }
            }
            _ => return Err("invalid checkpoint baseline origin".into()),
        };
        Ok(Self { worktree, baseline })
    }
}

fn canonical(path: &PathIdentity) -> Result<&str, &'static str> {
    path.as_canonical()
        .ok_or("checkpoint origin is not canonical")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pu_capture_origin_requires_explicit_matching_worktree_and_vcs_coordinates() {
        let root = if cfg!(windows) {
            "C:/origin-root"
        } else {
            "/origin-root"
        };
        let plain = json!({"worktree":{"kind":"plain","root":root},"baseline":{"kind":"plain"}});
        let decoded = Origin::decode(&plain).unwrap_or_else(|error| panic!("origin: {error}"));
        assert_eq!(
            decoded
                .encode()
                .unwrap_or_else(|error| panic!("encode: {error}")),
            plain
        );
        for changed in [
            json!({"worktree":plain["worktree"]}),
            json!({"worktree":null,"baseline":{"kind":"plain"}}),
            json!({"worktree":plain["worktree"],"baseline":{"kind":"unavailable"}}),
            json!({"worktree":plain["worktree"],"baseline":{"kind":"plain"},"extra":true}),
        ] {
            assert!(Origin::decode(&changed).is_err());
        }
        let git = json!({"worktree":{"kind":"git","root":root,"commonDir":root,"gitDir":root},"baseline":{"kind":"git","head":"0123456789012345678901234567890123456789","branch":{"kind":"symbolic","name":"dev"}}});
        let decoded = Origin::decode(&git).unwrap_or_else(|error| panic!("Git origin: {error}"));
        assert_eq!(
            decoded
                .encode()
                .unwrap_or_else(|error| panic!("Git encode: {error}")),
            git
        );
        let mut missing_head = git.clone();
        missing_head["baseline"]["head"] = Value::Null;
        assert!(Origin::decode(&missing_head).is_err());
        let mut mismatched = git;
        mismatched["baseline"] = plain["baseline"].clone();
        assert!(Origin::decode(&mismatched).is_err());
    }
}
