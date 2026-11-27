//! Optional display metadata from approved preimages and planned postimages.
//! No presentation failure changes whether the governed patch executes.
use super::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;

const SOURCE_BYTES: usize = 4 * 1024 * 1024;
const RECEIPT_BYTES: usize = 64 * 1024;
const SIDE_BYTES: usize = 8 * 1024;
const SIDE_LINES: usize = 128;

pub(super) fn prepare(root: &Path, plan: &PatchPlan) -> Result<Vec<Value>, String> {
    // The checkpoint owner already derives exact postimages for every step.
    // Reuse it rather than interpreting the patch a second way for display.
    let planned = receipt::planned_images(root, plan)?;
    let after = planned.postimages(usize::MAX, &[]);
    let mut originals: BTreeMap<&Path, Option<&str>> = BTreeMap::new();
    for step in &plan.0 {
        let (path, hash, destination) = match step {
            PatchStep::MakeDirectory { .. } => continue,
            PatchStep::Create { path, .. } => (path.as_path(), None, None),
            PatchStep::TextPatchWrite {
                path,
                before_sha256,
                ..
            } => (path.as_path(), before_sha256.as_deref(), None),
            PatchStep::Update {
                path,
                before_sha256,
                ..
            }
            | PatchStep::Delete {
                path,
                before_sha256,
            } => (path.as_path(), Some(before_sha256.as_str()), None),
            PatchStep::Rename {
                from,
                to,
                before_sha256,
            } => (
                from.as_path(),
                Some(before_sha256.as_str()),
                Some((to.as_path(), None)),
            ),
            PatchStep::TextPatchMove {
                from,
                to,
                before_sha256,
                destination_sha256,
                ..
            } => (
                from.as_path(),
                Some(before_sha256.as_str()),
                Some((to.as_path(), destination_sha256.as_deref())),
            ),
        };
        originals.entry(path).or_insert(hash);
        if let Some((path, hash)) = destination {
            originals.entry(path).or_insert(hash);
        }
    }
    let mut remaining = RECEIPT_BYTES.saturating_sub(originals.len() * 1024);
    let mut source_bytes = 0usize;
    let mut files = Vec::new();
    for (path, hash) in originals {
        let path = path
            .strip_prefix(root)
            .map_err(|_| "diff path escaped workspace")?;
        let before = if let Some(hash) = hash {
            let (_, bytes) = source_content(root, path.to_str().ok_or("diff path is not UTF-8")?)?;
            source_bytes = source_bytes.saturating_add(bytes.len());
            if source_bytes > SOURCE_BYTES {
                return Err("diff preimage byte limit".into());
            }
            if !sha256(&bytes).eq_ignore_ascii_case(hash) {
                return Err("diff preimage differs from approved snapshot".into());
            }
            Some(bytes)
        } else {
            None
        };
        let next = after.get(path).ok_or("diff postimage absent")?;
        let mut file = metadata(&path_text(path), before.as_deref(), *next);
        let bytes = serde_json::to_vec(&file).map_err(|e| e.to_string())?.len();
        if bytes > remaining {
            file["diff"] = Value::Null;
            file["patchOmitted"] = true.into();
            file["omittedReason"] = "diff receipt byte limit".into();
        }
        let bytes = serde_json::to_vec(&file).map_err(|e| e.to_string())?.len();
        remaining = remaining.saturating_sub(bytes);
        files.push(file);
    }
    Ok(files)
}

pub(in crate::dogfood::tools) fn metadata(
    path: &str,
    before: Option<&[u8]>,
    after: Option<&[u8]>,
) -> Value {
    let before = before.unwrap_or_default();
    let after = after.unwrap_or_default();
    let mut file = json!({"relativePath":path,"patchOmitted":true,"countsAvailable":false,
        "additions":null,"deletions":null,"diff":null});
    let (Ok(before), Ok(after)) = (std::str::from_utf8(before), std::str::from_utf8(after)) else {
        file["omittedReason"] = "binary preimage or postimage; no text diff".into();
        return file;
    };
    let old: Vec<_> = before.split_inclusive('\n').collect();
    let new: Vec<_> = after.split_inclusive('\n').collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    if let Some((removed, added)) = counts(
        &old[prefix..old.len() - suffix],
        &new[prefix..new.len() - suffix],
    ) {
        file["countsAvailable"] = true.into();
        file["additions"] = added.into();
        file["deletions"] = removed.into();
    }
    let first = prefix.saturating_sub(3);
    let old = &old[first..old.len() - suffix.saturating_sub(3)];
    let new = &new[first..new.len() - suffix.saturating_sub(3)];
    let preview = |lines: &[&str]| {
        let mut text = String::new();
        let mut shown = 0;
        for line in lines.iter().take(SIDE_LINES) {
            if text.len().saturating_add(line.len()) > SIDE_BYTES {
                break;
            }
            text.push_str(line);
            shown += 1;
        }
        (text, lines.len() - shown)
    };
    let (old, omitted_old) = preview(old);
    let (new, omitted_new) = preview(new);
    file["patchOmitted"] = (omitted_old + omitted_new > 0).into();
    file["diff"] = json!({"firstLine":first+1,"oldText":old,"newText":new,
        "omittedOldLines":omitted_old,"omittedNewLines":omitted_new});
    file
}

/// Exact counts after common ends, with bounded work; unknown stays unknown.
fn counts(old: &[&str], new: &[&str]) -> Option<(usize, usize)> {
    if old.is_empty() || new.is_empty() {
        return Some((old.len(), new.len()));
    }
    if old.len().saturating_mul(new.len()) > 40_000 {
        return None;
    }
    let mut common = vec![0usize; new.len() + 1];
    for a in old {
        let mut diagonal = 0;
        for (j, b) in new.iter().enumerate() {
            let previous = common[j + 1];
            common[j + 1] = if a == b {
                diagonal + 1
            } else {
                common[j + 1].max(common[j])
            };
            diagonal = previous;
        }
    }
    let count = common[new.len()];
    Some((old.len() - count, new.len() - count))
}
