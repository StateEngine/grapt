// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// Modified by Grapt: bounded best-match retention with native relative paths.
// Path scoring/order.
//! Pure path scoring and bounded best-match retention.
use super::*;
use nucleo::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32Str};
use std::path::Component;

pub(super) struct Ranker<'a> {
    request: &'a SearchRequest,
    pattern: Pattern,
    matcher: Matcher,
    utf32: Vec<char>,
    pub(super) results: SearchResults,
}
impl<'a> Ranker<'a> {
    pub(super) fn new(request: &'a SearchRequest) -> Self {
        Self {
            request,
            pattern: Pattern::new(
                &request.query,
                CaseMatching::Smart,
                Normalization::Smart,
                AtomKind::Fuzzy,
            ),
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            utf32: Vec::new(),
            results: SearchResults {
                matches: Vec::with_capacity(request.limits.results),
                scanned_entries: 0,
                matching_paths: 0,
                omitted_paths: 0,
                entry_limit_reached: false,
                depth_limit_reached: false,
                current: request.current.clone(),
            },
        }
    }
    pub(super) fn offer(&mut self, path: PathBuf) -> Result<(), SearchError> {
        if !self.request.is_current() {
            return Ok(());
        }
        if path.as_os_str().len() > MAX_PATH_BYTES {
            self.results.omitted_paths += 1;
            return Ok(());
        }
        if path.as_os_str().is_empty()
            || path
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(SearchError::Candidate);
        }
        if self
            .results
            .matches
            .iter()
            .any(|row| row.relative_path == path)
        {
            return Ok(());
        }
        let Some(text) = path.to_str().filter(|text| text.len() <= MAX_PATH_BYTES) else {
            self.results.omitted_paths += 1;
            return Ok(());
        };
        // Use the native PathBuf for selection, a stable slash spelling only
        // for scoring. A Windows separator never changes a Unix backslash name.
        let display = if std::path::MAIN_SEPARATOR == '\\' {
            text.replace('\\', "/")
        } else {
            text.to_owned()
        };
        let Some(score) = self
            .pattern
            .score(Utf32Str::new(&display, &mut self.utf32), &mut self.matcher)
        else {
            return Ok(());
        };
        self.results.matching_paths += 1;
        let candidate = FileMatch {
            relative_path: path,
            score,
        };
        let at = self.results.matches.partition_point(|held| {
            held.score > score
                || (held.score == score && held.relative_path < candidate.relative_path)
        });
        if at >= self.request.limits.results {
            return Ok(());
        }
        if self.results.matches.len() == self.request.limits.results {
            self.results.matches.pop();
        }
        self.results.matches.insert(at, candidate);
        Ok(())
    }
    pub(super) fn finish(self) -> SearchOutcome {
        if self.request.is_current() {
            SearchOutcome::Ready(self.results)
        } else {
            SearchOutcome::Cancelled
        }
    }
}
