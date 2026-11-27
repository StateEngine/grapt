//! Bounded directory listing with the same ignore policy as search.

use super::*;

pub fn list(
    root: &Path,
    directory: &Path,
    limit: u64,
    include_ignored: bool,
) -> Result<Vec<u8>, String> {
    let relative = directory
        .strip_prefix(root)
        .map_err(|_| "list directory escaped workspace".to_owned())?;
    reject_links(root, relative)?;
    let policy = super::scope_policy(root, directory, include_ignored, &mut 0, &mut 0)?;
    if let Some((path, reason)) = policy.exclusion {
        let path = path
            .strip_prefix(root)
            .map_err(|_| "list scope escaped workspace")?;
        return encode(&serde_json::json!({
            "entries":[], "ignoredCount":1, "truncated":false, "complete":false,
            "skipped":[{"path":path.to_string_lossy().replace('\\', "/"), "reason":reason}],
        }));
    }
    let rules = policy.rules;
    let read = std::fs::read_dir(directory).map_err(|error| {
        format!(
            "list directory unavailable ({}): {error}",
            directory.display()
        )
    })?;
    let mut entries = Vec::new();
    let mut skipped = Vec::new();
    let mut ignored_count = 0;
    let mut visited = 0;
    for entry in read {
        visited += 1;
        if visited > 10_000 {
            return Err("list entry bound exceeded; narrow path".into());
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                skipped.push(serde_json::json!({"reason":format!("entry unreadable: {error}")}));
                continue;
            }
        };
        let path = entry.path();
        let name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => {
                skipped.push(serde_json::json!({"reason":"filename-not-utf8"}));
                continue;
            }
        };
        if super::excluded(&name) {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                skipped.push(serde_json::json!({"path":name,"reason":format!("metadata unreadable: {error}")}));
                continue;
            }
        };
        if (metadata.is_dir() && ignore_rules::build_directory(&name))
            || (!include_ignored && super::ignored(&rules, &path, &name, metadata.is_dir()))
        {
            ignored_count += 1;
            continue;
        }
        let kind = if is_link(&metadata) {
            "symlink"
        } else if metadata.is_dir() {
            "directory"
        } else if metadata.is_file() {
            "file"
        } else {
            "other"
        };
        let workspace_path = path
            .strip_prefix(root)
            .map_err(|_| "list entry escaped workspace".to_owned())?
            .to_str()
            .ok_or("list path is not UTF-8")?
            .replace('\\', "/");
        if workspace_path.len() > PATH_BYTES {
            return Err("list path byte bound exceeded".into());
        }
        entries.push(serde_json::json!({"path":workspace_path,"kind":kind}));
    }
    entries.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
    let truncated = entries.len() as u64 > limit;
    entries.truncate(usize::try_from(limit).map_err(|_| "list limit exceeds usize")?);
    encode(&serde_json::json!({
        "entries":entries,
        "ignoredCount":ignored_count,
        "skipped":skipped,
        "truncated":truncated,
        "complete":!truncated && skipped.is_empty(),
    }))
}
