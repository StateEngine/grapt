//! A read-only draft view for the TUI's integration tests, compiled only for
//! this crate's tests and the default-off `test-support` feature.
use super::Board;
use grapt_client::wire::FormValue;
use std::collections::BTreeMap;

impl Board {
    /// The canonical settlement held by a draft, without exposing edit state.
    pub fn draft_state(&self, index: usize) -> Option<&grapt_client::wire::FormState> {
        self.drafts
            .get(index)
            .and_then(|draft| draft.state.as_ref())
    }

    /// The values drafted so far for the draft at `index`, if there is one.
    pub fn draft_values(&self, index: usize) -> Option<&BTreeMap<String, FormValue>> {
        self.drafts.get(index).map(|draft| &draft.values)
    }
}
