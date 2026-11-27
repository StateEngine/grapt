//! Bounded, engine-supplied preimage/postimage previews of completed patches.
use super::*;
use serde_json::Value;

pub(super) fn changes(output: &str) -> Option<Vec<Change>> {
    let output: Value = serde_json::from_str(output).ok()?;
    let files = output.get("files")?.as_array()?;
    if files.is_empty() || files.len() > 32 {
        return None;
    }
    // Legacy edit/write receipts carry only unified patches; keep their parser.
    if files.iter().any(|file| file.get("diff").is_none()) {
        return None;
    }
    files.iter().map(file).collect()
}

fn file(file: &Value) -> Option<Change> {
    let path = file.get("relativePath")?.as_str()?;
    if path.len() > 4096 {
        return None;
    }
    let mut change = if let Some(diff) = file.get("diff").filter(|d| d.is_object()) {
        let first = usize::try_from(diff.get("firstLine")?.as_u64()?).ok()?;
        let old = diff.get("oldText")?.as_str()?;
        let new = diff.get("newText")?.as_str()?;
        let omitted_old = usize::try_from(diff.get("omittedOldLines")?.as_u64()?).ok()?;
        let omitted_new = usize::try_from(diff.get("omittedNewLines")?.as_u64()?).ok()?;
        if first == 0 || old.len().saturating_add(new.len()) > 16 * 1024 {
            return None;
        }
        first.checked_add(old.lines().count().max(new.lines().count()))?;
        let mut change = compare(shown(path), old, new, Some(first));
        change.rows.hidden = change
            .rows
            .hidden
            .saturating_add(omitted_old)
            .saturating_add(omitted_new);
        change
    } else {
        Change::bare(Action::Edit, format!("{} (diff unavailable)", shown(path)))
    };
    change.counts_known = file.get("countsAvailable") == Some(&Value::Bool(true));
    if change.counts_known {
        change.added = usize::try_from(file.get("additions")?.as_u64()?).ok()?;
        change.removed = usize::try_from(file.get("deletions")?.as_u64()?).ok()?;
    }
    Some(change)
}
