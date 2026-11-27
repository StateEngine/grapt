// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// Modified by Grapt. Ignore-source selection.
//! Bounded ignore metadata; paths and pattern contents never become tool output.
use super::*;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::path::Component;
use std::sync::Arc;

pub(super) struct Rules {
    local: Option<Gitignore>,
    git: Option<Gitignore>,
    exclude: Option<Gitignore>,
}

pub(super) fn ignored(rules: &[Arc<Rules>], path: &Path, name: &str, directory: bool) -> bool {
    // Source kind wins before depth: a parent .ignore overrides a child
    // .gitignore. Within each kind the deepest/latest matching rule wins.
    for kind in 0..3 {
        for rules in rules.iter().rev() {
            let matcher = match kind {
                0 => &rules.local,
                1 => &rules.git,
                _ => &rules.exclude,
            };
            if let Some(matcher) = matcher {
                let found = matcher.matched(path, directory);
                if found.is_whitelist() {
                    return false;
                }
                if found.is_ignore() {
                    return true;
                }
            }
        }
    }
    name.starts_with('.')
}

pub(super) fn build_directory(name: &str) -> bool {
    [
        "target",
        "node_modules",
        "dist",
        "build",
        ".next",
        ".nuxt",
        "__pycache__",
        ".venv",
        "venv",
    ]
    .iter()
    .any(|noise| {
        if cfg!(windows) {
            name.eq_ignore_ascii_case(noise)
        } else {
            name == *noise
        }
    })
}

pub(super) fn load(
    directory: &Path,
    count: &mut usize,
    bytes: &mut usize,
) -> Result<Option<Rules>, String> {
    let exclude = match git_exclude(directory, count, bytes)? {
        Some(path) if unlinked(&path)? => {
            matcher(directory, &path, ".git/info/exclude", count, bytes)?
        }
        _ => None,
    };
    let git = matcher(
        directory,
        &directory.join(".gitignore"),
        ".gitignore",
        count,
        bytes,
    )?;
    let local = matcher(
        directory,
        &directory.join(".ignore"),
        ".ignore",
        count,
        bytes,
    )?;
    Ok(
        (local.is_some() || git.is_some() || exclude.is_some()).then_some(Rules {
            local,
            git,
            exclude,
        }),
    )
}

fn matcher(
    root: &Path,
    path: &Path,
    label: &str,
    count: &mut usize,
    bytes: &mut usize,
) -> Result<Option<Gitignore>, String> {
    let Some(content) = text(path, label, 64 * 1024, count, bytes)? else {
        return Ok(None);
    };
    let mut builder = GitignoreBuilder::new(root);
    for (index, line) in content.lines().enumerate() {
        if index >= 4096 {
            return Err(format!("ignore line bound exceeded at {label}"));
        }
        // Parser errors can quote the pattern, including repository-private
        // info/exclude content. Report its source and line without the bytes.
        builder
            .add_line(Some(path.to_path_buf()), line)
            .map_err(|_| format!("ignore pattern invalid ({label}:{})", index + 1))?;
    }
    builder
        .build()
        .map(Some)
        .map_err(|_| format!("ignore matcher invalid ({label})"))
}

fn text(
    path: &Path,
    label: &str,
    maximum: u64,
    count: &mut usize,
    total: &mut usize,
) -> Result<Option<String>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("ignore metadata unreadable ({label}): {error}")),
    };
    if !metadata.is_file() || is_link(&metadata) {
        return Err(format!(
            "ignore file is not a regular unlinked file: {label}"
        ));
    }
    *count += 1;
    if *count > 64 || metadata.len() > maximum {
        return Err(format!("ignore file count/byte bound exceeded at {label}"));
    }
    let remaining = (512 * 1024_usize).saturating_sub(*total);
    let bytes = read_bounded(path, maximum.min(remaining as u64))
        .map_err(|error| format!("ignore file ({label}): {error}"))?;
    *total += bytes.len();
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| format!("ignore file is not UTF-8 ({label})"))
}

fn git_exclude(
    directory: &Path,
    count: &mut usize,
    bytes: &mut usize,
) -> Result<Option<PathBuf>, String> {
    let marker = directory.join(".git");
    let metadata = match std::fs::symlink_metadata(&marker) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Git ignore metadata unreadable: {error}")),
    };
    if is_link(&metadata) {
        return Err("Git ignore metadata is linked".into());
    }
    let git = if metadata.is_dir() {
        marker
    } else {
        let pointer =
            text(&marker, ".git", 4096, count, bytes)?.ok_or("Git ignore pointer disappeared")?;
        let target = pointer
            .trim_end_matches(['\r', '\n'])
            .strip_prefix("gitdir: ")
            .ok_or("Git ignore pointer is malformed")?;
        metadata_path(directory, target)?
    };
    if !unlinked(&git)? {
        return Ok(None);
    }
    let common = match text(&git.join("commondir"), "Git commondir", 4096, count, bytes)? {
        Some(pointer) => metadata_path(&git, pointer.trim_end_matches(['\r', '\n']))?,
        None => git,
    };
    Ok(Some(common.join("info/exclude")))
}

fn metadata_path(base: &Path, value: &str) -> Result<PathBuf, String> {
    if value.is_empty() || value.contains(['\0', '\r', '\n']) {
        return Err("Git ignore metadata path is malformed".into());
    }
    let path = base.join(value);
    if path.as_os_str().len() > 4096 || path.components().count() > 64 {
        return Err("Git ignore metadata path exceeds its bound".into());
    }
    #[cfg(windows)]
    if path.components().any(|component| matches!(component, Component::Prefix(prefix)
        if !matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))) {
        return Err("Git ignore metadata requires a local disk path".into());
    }
    Ok(path)
}

fn unlinked(path: &Path) -> Result<bool, String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        if !matches!(component, Component::Normal(_) | Component::ParentDir) {
            continue;
        }
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if is_link(&metadata) => {
                return Err("Git ignore metadata is linked".into())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(format!("Git ignore metadata unreadable: {error}")),
        }
    }
    Ok(true)
}
