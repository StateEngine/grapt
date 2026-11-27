// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// Modified by Grapt: bounded selected-session DTO history, synchronous local
// traversal, no history file or effects. Navigation/search semantics.
//! SC-TUI-02: recall never submits; leaving a preview restores the draft.
use std::collections::VecDeque;

const MAX_ENTRIES: usize = 128;
const MAX_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_QUERY_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub text: String,
    pub cursor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchStatus {
    Match,
    NoMatch,
    Boundary,
}

#[derive(Debug)]
struct Navigation {
    index: usize,
    original: Draft,
}

#[derive(Debug)]
struct Search {
    original: Draft,
    query: String,
    query_lower: String,
    selected: Option<usize>,
    status: SearchStatus,
}

#[derive(Debug, Default)]
pub struct History {
    entries: VecDeque<String>,
    omitted: usize,
    navigation: Option<Navigation>,
    search: Option<Search>,
}

impl History {
    /// Source order is newest first. Never recall a partial prompt.
    /// An active preview freezes its snapshot until accepted or canceled.
    pub fn refresh<'a>(&mut self, newest_first: impl Iterator<Item = &'a str>) {
        if self.navigation.is_some() || self.search.is_some() {
            return;
        }
        self.entries.clear();
        self.omitted = 0;
        let mut bytes = 0usize;
        for text in newest_first {
            if text.is_empty() {
                continue;
            }
            if self.entries.len() == MAX_ENTRIES || bytes.saturating_add(text.len()) > MAX_BYTES {
                self.omitted = self.omitted.saturating_add(1);
                continue;
            }
            bytes += text.len();
            self.entries.push_front(text.to_owned());
        }
    }

    pub fn omitted(&self) -> usize {
        self.omitted
    }
    pub fn searching(&self) -> bool {
        self.search.is_some()
    }
    pub fn query(&self) -> Option<&str> {
        self.search.as_ref().map(|search| search.query.as_str())
    }
    pub fn status(&self) -> Option<SearchStatus> {
        self.search.as_ref().map(|search| search.status)
    }

    /// Edits detach a recalled entry; Up/Down then keep normal cursor behavior.
    pub fn edited(&mut self) {
        self.navigation = None;
    }

    pub fn should_navigate(&self, text: &str, cursor: usize) -> bool {
        if self.searching() || self.entries.is_empty() {
            return false;
        }
        text.is_empty()
            || ((cursor == 0 || cursor == text.len())
                && self
                    .navigation
                    .as_ref()
                    .and_then(|navigation| self.entries.get(navigation.index))
                    .is_some_and(|last| last == text))
    }

    pub fn older(&mut self, text: &str, cursor: usize) -> Option<Draft> {
        if !self.should_navigate(text, cursor) {
            return None;
        }
        let index = match self.navigation.as_ref() {
            Some(navigation) => navigation.index.checked_sub(1)?,
            None => self.entries.len().checked_sub(1)?,
        };
        let text = self.entries.get(index)?.clone();
        match self.navigation.as_mut() {
            Some(navigation) => navigation.index = index,
            None => {
                self.navigation = Some(Navigation {
                    index,
                    original: Draft {
                        text: String::new(),
                        cursor: 0,
                    },
                })
            }
        }
        Some(Draft {
            cursor: text.len(),
            text,
        })
    }

    pub fn newer(&mut self, text: &str, cursor: usize) -> Option<Draft> {
        if !self.should_navigate(text, cursor) {
            return None;
        }
        let index = self.navigation.as_ref()?.index + 1;
        if index >= self.entries.len() {
            return self.navigation.take().map(|navigation| navigation.original);
        }
        self.navigation.as_mut()?.index = index;
        let text = self.entries.get(index)?.clone();
        Some(Draft {
            cursor: text.len(),
            text,
        })
    }

    pub fn begin_search(&mut self, text: &str, cursor: usize) -> Option<Draft> {
        let original = Draft {
            text: text.to_owned(),
            cursor,
        };
        self.navigation = None;
        let selected = self.entries.len().checked_sub(1);
        self.search = Some(Search {
            original,
            query: String::new(),
            query_lower: String::new(),
            selected,
            status: if selected.is_some() {
                SearchStatus::Match
            } else {
                SearchStatus::NoMatch
            },
        });
        self.selected()
    }

    /// False refuses an oversized query without changing selection or draft.
    pub fn set_query(&mut self, query: String) -> bool {
        if query.len() > MAX_QUERY_BYTES {
            return false;
        }
        let Some(search) = self.search.as_mut() else {
            return false;
        };
        search.query_lower = query.to_lowercase();
        search.query = query;
        search.selected = self
            .entries
            .iter()
            .rposition(|text| text.to_lowercase().contains(&search.query_lower));
        search.status = if search.selected.is_some() {
            SearchStatus::Match
        } else {
            SearchStatus::NoMatch
        };
        true
    }

    pub fn search_older(&mut self) -> Option<Draft> {
        let search = self.search.as_mut()?;
        let start = search.selected?;
        let found = (0..start).rev().find(|&index| {
            let text = &self.entries[index];
            text.to_lowercase().contains(&search.query_lower)
                && !self
                    .entries
                    .iter()
                    .skip(index + 1)
                    .any(|newer| newer == text)
        });
        match found {
            Some(index) => {
                search.selected = Some(index);
                search.status = SearchStatus::Match;
            }
            None => search.status = SearchStatus::Boundary,
        }
        self.selected()
    }

    pub fn selected(&self) -> Option<Draft> {
        let text = self.entries.get(self.search.as_ref()?.selected?)?.clone();
        Some(Draft {
            cursor: text.len(),
            text,
        })
    }

    pub fn search_newer(&mut self) -> Option<Draft> {
        let search = self.search.as_mut()?;
        let start = search.selected?.saturating_add(1);
        let found = (start..self.entries.len()).find(|&index| {
            let text = &self.entries[index];
            text.to_lowercase().contains(&search.query_lower)
                && !self
                    .entries
                    .iter()
                    .skip(index + 1)
                    .any(|newer| newer == text)
        });
        match found {
            Some(index) => {
                search.selected = Some(index);
                search.status = SearchStatus::Match;
            }
            None => search.status = SearchStatus::Boundary,
        }
        self.selected()
    }

    /// Accepting leaves matching text editable; the controller does not submit.
    pub fn accept_search(&mut self) -> Option<Draft> {
        let selected = self.selected();
        let search = self.search.take()?;
        Some(selected.unwrap_or(search.original))
    }

    /// Used by Escape and session changes, before replacing the selected history.
    pub fn cancel(&mut self) -> Option<Draft> {
        self.search
            .take()
            .map(|search| search.original)
            .or_else(|| self.navigation.take().map(|navigation| navigation.original))
    }
}

#[cfg(test)]
mod tests;
