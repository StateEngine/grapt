//! Shared read-only presentation of the backend workspace diff snapshot.
use super::visible;
use crate::client::{Client, Connection};
pub fn lines<C: Connection>(client: &Client<C>, width: usize) -> Vec<String> {
    let mut lines = vec!["WORKSPACE DIFF".to_owned()];
    let text = match client.workspace_diff() {
        Ok(Some(diff)) if !diff.is_git => "Not inside a Git repository.",
        Ok(Some(diff)) if diff.text.is_empty() => "No staged, unstaged or untracked changes.",
        Ok(Some(diff)) => diff.text.as_str(),
        Ok(None) => "Diff not read.",
        Err(error) => error,
    };
    for line in text.lines() {
        lines.extend(crate::cells::wrap(&visible(line), width));
    }
    lines
}
