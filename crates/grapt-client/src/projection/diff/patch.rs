//! Display-only parsing of the text patch input. Never decides whether it applied.
//! bb64b894fd67158be0f874872e6d7bfe717e7368, Apache-2.0, Copyright 2025 OpenAI.
use super::*;

pub(super) fn parse(input: &str) -> Option<Vec<Change>> {
    let mut lines = input.lines();
    if lines.next()? != "*** Begin Patch" {
        return None;
    }
    let mut changes = Vec::new();
    let mut current: Option<Change> = None;
    for text in lines {
        let file = text
            .strip_prefix("*** Add File: ")
            .map(|p| (Action::Add, p))
            .or_else(|| {
                text.strip_prefix("*** Update File: ")
                    .map(|p| (Action::Edit, p))
            })
            .or_else(|| {
                text.strip_prefix("*** Delete File: ")
                    .map(|p| (Action::Delete, p))
            });
        if let Some((action, path)) = file {
            if let Some(change) = current.take() {
                changes.push(change);
            }
            if changes.len() == 64 {
                return None;
            }
            let mut change = Change::bare(action, shown(path));
            change.counts_known = false;
            current = Some(change);
            continue;
        }
        if text == "*** End Patch" {
            if let Some(change) = current {
                changes.push(change);
            }
            return Some(changes);
        }
        let change = current.as_mut()?;
        if let Some(path) = text.strip_prefix("*** Move to: ") {
            change.path.push_str(&format!(" → {}", shown(path)));
        } else if text.starts_with("@@") {
            if !change.rows.shown.is_empty() {
                change.rows.push(|| Row::Gap);
            }
        } else if text == "*** End of File" {
            continue;
        } else {
            let (mark, raw) = match text.as_bytes().first() {
                Some(b'+') => {
                    change.added += 1;
                    (Mark::Insert, &text[1..])
                }
                Some(b'-') => {
                    change.removed += 1;
                    (Mark::Delete, &text[1..])
                }
                Some(b' ') => (Mark::Context, &text[1..]),
                _ => return None,
            };
            let number = matches!(change.action, Action::Add).then_some(change.added);
            change.rows.push(|| Row::Line {
                old: None,
                new: number,
                mark,
                text: shown(raw),
                ended: true,
            });
        }
    }
    None
}
