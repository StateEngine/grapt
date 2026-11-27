//! Git discovery ceilings also apply to the walker's filesystem discovery.
use super::{Path, PathBuf, SearchError};
use std::collections::BTreeMap;

pub(super) struct Rules {
    pub(super) parents: bool,
    root: PathBuf,
    repositories: BTreeMap<PathBuf, ignore::IncrementalIgnore>,
}

impl Rules {
    pub(super) fn from_environment(root: &Path) -> Result<Self, SearchError> {
        let raw = std::env::var_os("GIT_CEILING_DIRECTORIES").unwrap_or_default();
        if raw.len() > 16_384 {
            return Err(SearchError::Walk(
                "Git discovery ceilings exceed their byte bound".into(),
            ));
        }
        let ceilings: Vec<_> = std::env::split_paths(&raw).take(65).collect();
        if ceilings.len() > 64 {
            return Err(SearchError::Walk(
                "Git discovery exceeds its ceiling count bound".into(),
            ));
        }
        Self::below(root, &ceilings)
    }

    pub(super) fn below(root: &Path, ceilings: &[PathBuf]) -> Result<Self, SearchError> {
        let coordinate = std::fs::canonicalize(root).map_err(SearchError::RootRead)?;
        let mut normalized = Vec::new();
        for ceiling in ceilings.iter().filter(|path| path.is_absolute()) {
            match std::fs::canonicalize(ceiling) {
                Ok(path) => normalized.push(path),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(SearchError::RootRead(error)),
            }
        }
        let mut parents = true;
        for (index, parent) in coordinate.ancestors().enumerate() {
            if index >= 256 {
                return Err(SearchError::Walk(
                    "Git discovery exceeds its ancestor bound".into(),
                ));
            }
            if index > 0
                && normalized
                    .iter()
                    .any(|ceiling| ceiling.is_absolute() && ceiling == parent)
            {
                parents = false;
                break;
            }
            if has_repository(parent)? {
                break;
            }
        }
        Ok(Self {
            parents,
            root: root.to_owned(),
            repositories: BTreeMap::new(),
        })
    }

    pub(super) fn includes(&mut self, path: &Path, directory: bool) -> Result<bool, SearchError> {
        if self.parents {
            return Ok(true);
        }
        // WalkBuilder's parent switch does not stop its repository discovery.
        // A plain root uses only .ignore. Enable Git rules separately inside
        // actual nested repositories, without trusting markers above the root.
        // Ignored descendants still count against the existing walk entry cap.
        let start = if directory {
            path
        } else {
            path.parent().ok_or(SearchError::Candidate)?
        };
        let mut repository = None;
        for parent in start.ancestors().take(256) {
            if !parent.starts_with(&self.root) {
                break;
            }
            if has_repository(parent)? {
                repository = Some(parent);
            }
        }
        // Start at the outer repository: an ignored directory stays excluded
        // even if it contains another .git marker. The matcher handles nested
        // boundaries only after their enclosing directory has been admitted.
        if let Some(parent) = repository {
            if !self.repositories.contains_key(parent) {
                let matcher = ignore::WalkBuilder::new(parent)
                    .hidden(false)
                    .parents(false)
                    .require_git(true)
                    .ignore(false)
                    .build_matchers()
                    .pop()
                    .ok_or_else(|| SearchError::Walk("Git ignore matcher is absent".into()))?;
                self.repositories.insert(parent.to_owned(), matcher);
            }
            let matcher = self
                .repositories
                .get_mut(parent)
                .ok_or_else(|| SearchError::Walk("Git ignore matcher is absent".into()))?;
            let relative = path
                .strip_prefix(parent)
                .map_err(|_| SearchError::Candidate)?;
            let (matched, error) = matcher.matched_with_errors(relative, directory);
            if let Some(error) = error {
                return Err(SearchError::Walk(super::bounded_reason(&error)));
            }
            return Ok(!matched.is_ignore());
        }
        Ok(true)
    }
}

fn has_repository(path: &Path) -> Result<bool, SearchError> {
    match std::fs::symlink_metadata(path.join(".git")) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(SearchError::RootRead(error)),
    }
}
