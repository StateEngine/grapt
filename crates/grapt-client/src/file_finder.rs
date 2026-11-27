// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// Modified by Grapt: bounded sequential core, one cancellation stamp, no UI.
//! Mention file search. Run on the caller's bounded worker; begin/cancel do no I/O.
//! Results are suggestions, not permission or proof a later file read is safe.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use thiserror::Error;
mod parents;
mod ranking;
use ranking::Ranker;

pub const MAX_QUERY_BYTES: usize = 256;
pub const MAX_PATH_BYTES: usize = 4_096;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    results: usize,
    entries: usize,
    depth: usize,
}
impl Limits {
    pub const PRODUCTION: Self = Self {
        results: 64,
        entries: 10_000,
        depth: 32,
    };
    pub fn new(results: usize, entries: usize, depth: usize) -> Result<Self, SearchError> {
        if !(1..=100).contains(&results)
            || !(1..=100_000).contains(&entries)
            || !(1..=64).contains(&depth)
        {
            return Err(SearchError::Limits);
        }
        Ok(Self {
            results,
            entries,
            depth,
        })
    }
}
#[derive(Debug, Error)]
pub enum SearchError {
    #[error("file search query exceeds its byte bound or contains controls")]
    Query,
    #[error("file search root must be an absolute directory within the path bound")]
    Root,
    #[error("file search limits are outside their admitted bounds")]
    Limits,
    #[error("file search candidate is not a relative path within the root")]
    Candidate,
    #[error("file search could not read its root: {0}")]
    RootRead(#[source] std::io::Error),
    #[error("file search could not walk the repository: {0}")]
    Walk(String),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMatch {
    pub relative_path: PathBuf,
    pub score: u32,
}
#[derive(Debug)]
pub struct SearchResults {
    pub matches: Vec<FileMatch>,
    pub scanned_entries: usize,
    pub matching_paths: usize,
    pub omitted_paths: usize,
    pub entry_limit_reached: bool,
    pub depth_limit_reached: bool,
    current: Arc<AtomicBool>,
}
impl SearchResults {
    /// Check at publication too: a newer query may arrive after run returned.
    pub fn is_current(&self) -> bool {
        !self.current.load(Ordering::Acquire)
    }
    pub fn results_limited(&self) -> bool {
        self.matching_paths > self.matches.len()
    }
}
#[derive(Debug)]
pub enum SearchOutcome {
    Ready(SearchResults),
    Cancelled,
}

/// Owns one active stamp, not a worker, queue, directory index or result cache.
#[derive(Debug, Default)]
pub struct SearchController {
    active: Option<Arc<AtomicBool>>,
}
impl SearchController {
    /// Every edit invalidates old work, even if the new query is refused.
    /// An empty query lists a bounded set; no mention token uses cancel().
    pub fn begin(
        &mut self,
        root: &Path,
        query: &str,
        limits: Limits,
    ) -> Result<SearchRequest, SearchError> {
        self.cancel();
        if query.len() > MAX_QUERY_BYTES || query.chars().any(char::is_control) {
            return Err(SearchError::Query);
        }
        if !root.is_absolute() || root.as_os_str().len() > MAX_PATH_BYTES {
            return Err(SearchError::Root);
        }
        let current = Arc::new(AtomicBool::new(false));
        self.active = Some(current.clone());
        Ok(SearchRequest {
            root: root.to_owned(),
            query: query.to_owned(),
            limits,
            current,
        })
    }
    pub fn cancel(&mut self) {
        if let Some(current) = self.active.take() {
            current.store(true, Ordering::Release);
        }
    }
}
impl Drop for SearchController {
    fn drop(&mut self) {
        self.cancel();
    }
}
#[derive(Debug)]
pub struct SearchRequest {
    root: PathBuf,
    query: String,
    limits: Limits,
    current: Arc<AtomicBool>,
}
impl SearchRequest {
    pub fn is_current(&self) -> bool {
        !self.current.load(Ordering::Acquire)
    }
    /// Filesystem-independent ranking for an already enumerated set. Both
    /// enumeration and retained results remain bounded, even for an endless iterator.
    pub fn rank_paths(
        &self,
        paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<SearchOutcome, SearchError> {
        let mut ranker = Ranker::new(self);
        let mut paths = paths.into_iter();
        for _ in 0..self.limits.entries {
            if !self.is_current() {
                return Ok(SearchOutcome::Cancelled);
            }
            let Some(path) = paths.next() else {
                return Ok(ranker.finish());
            };
            ranker.results.scanned_entries += 1;
            ranker.offer(path)?;
        }
        ranker.results.entry_limit_reached = true;
        Ok(ranker.finish())
    }
    /// Read file names only. Never follow child symlinks or offer .git metadata.
    /// Ignore errors are explicit; no partial successful result hides them.
    /// Git ignore rules require a repository and stop at its root; `.ignore`
    /// also applies outside repositories, following the walker's standard rules.
    pub fn search(&self) -> Result<SearchOutcome, SearchError> {
        if !self.is_current() {
            return Ok(SearchOutcome::Cancelled);
        }
        self.search_with_rules(parents::Rules::from_environment(&self.root)?)
    }
    fn search_with_rules(&self, mut rules: parents::Rules) -> Result<SearchOutcome, SearchError> {
        if !std::fs::metadata(&self.root)
            .map_err(SearchError::RootRead)?
            .is_dir()
        {
            return Err(SearchError::Root);
        }
        let mut walk = ignore::WalkBuilder::new(&self.root)
            .hidden(false)
            .follow_links(false)
            .require_git(true)
            .parents(rules.parents)
            .git_ignore(rules.parents)
            .git_global(rules.parents)
            .git_exclude(rules.parents)
            .max_depth(Some(self.limits.depth))
            .filter_entry(|entry| {
                !entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case(".git"))
            })
            .build();
        let mut ranker = Ranker::new(self);
        while ranker.results.scanned_entries < self.limits.entries {
            if !self.is_current() {
                return Ok(SearchOutcome::Cancelled);
            }
            let Some(entry) = walk.next() else {
                return Ok(ranker.finish());
            };
            ranker.results.scanned_entries += 1;
            let entry = entry.map_err(|error| SearchError::Walk(bounded_reason(&error)))?;
            if entry.error().is_some() {
                return Err(SearchError::Walk(
                    "repository ignore rules could not be read".into(),
                ));
            }
            let directory = entry.file_type().is_some_and(|kind| kind.is_dir());
            if !rules.includes(entry.path(), directory)? {
                continue;
            }
            if directory {
                if entry.depth() >= self.limits.depth {
                    ranker.results.depth_limit_reached = true;
                }
                continue;
            }
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let path = entry
                .path()
                .strip_prefix(&self.root)
                .map_err(|_| SearchError::Candidate)?;
            ranker.offer(path.to_owned())?;
        }
        ranker.results.entry_limit_reached = true;
        Ok(ranker.finish())
    }
}
fn bounded_reason(error: &impl std::fmt::Display) -> String {
    use std::fmt::Write;
    struct Reason(String);
    impl std::fmt::Write for Reason {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            let mut end = text.len().min(509 - self.0.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            self.0.push_str(&text[..end]);
            if end < text.len() {
                Err(std::fmt::Error)
            } else {
                Ok(())
            }
        }
    }
    let mut reason = Reason(String::with_capacity(512));
    if write!(&mut reason, "{error}").is_err() {
        reason.0.push_str("...");
    }
    reason.0
}
#[cfg(test)]
mod tests;
