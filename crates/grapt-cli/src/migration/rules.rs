//! Check the native directory as it will exist, including retained declarations.
use super::{files, graph, metadata};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

pub(super) fn markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

pub(super) fn validate(
    roots: &[(PathBuf, PathBuf, bool)],
    writes: &[(PathBuf, Vec<u8>)],
) -> Result<(), String> {
    let mut directories = BTreeSet::new();
    for (path, scope, rule) in roots {
        if *rule {
            directories.insert((
                path.parent().ok_or("rule has no directory")?.to_path_buf(),
                scope.clone(),
            ));
        }
    }
    let mut total_files = 0usize;
    let mut total_input = 0usize;
    let mut total_expanded = 0usize;
    let mut total_entries = 0usize;
    for (directory, scope) in directories {
        let mut entries = BTreeSet::new();
        if files::exists(&directory)? {
            for entry in
                std::fs::read_dir(&directory).map_err(|_| "native rule directory is unreadable")?
            {
                entries.insert(
                    entry
                        .map_err(|_| "native rule directory entry is unreadable")?
                        .path(),
                );
                if entries.len() > 256 {
                    return Err("post-import native rules exceed 256 directory entries".into());
                }
            }
        }
        for (path, _) in writes {
            if path.parent() == Some(directory.as_path()) {
                entries.insert(path.clone());
            }
        }
        total_entries = total_entries.saturating_add(entries.len());
        if total_entries > 256 {
            return Err("post-import native rules exceed 256 directory entries".into());
        }
        for path in entries.into_iter().filter(|path| markdown(path)) {
            total_files += 1;
            if total_files > 64 {
                return Err("post-import native rules exceed 64 Markdown files".into());
            }
            let bytes = if let Some((_, bytes)) = writes.iter().find(|(target, _)| *target == path)
            {
                bytes.clone()
            } else {
                let resolved = std::fs::canonicalize(&path)
                    .map_err(|_| "existing native rule is unresolved")?;
                if !resolved.starts_with(&scope) {
                    return Err("existing native rule escapes its scope".into());
                }
                files::read(&resolved)?.ok_or("existing native rule disappeared")?
            };
            total_input = total_input.saturating_add(bytes.len());
            if total_input > 1024 * 1024 {
                return Err("post-import native rule source input exceeds 1 MiB".into());
            }
            if !metadata::scoped(
                std::str::from_utf8(&bytes).map_err(|_| "native rule is not UTF-8")?,
            )? {
                return Err("native scoped rule requires paths or globs frontmatter".into());
            }
            total_expanded =
                total_expanded.saturating_add(graph::validate(&path, &scope, true, writes)?);
            if total_expanded > 65_536 {
                return Err("post-import scoped rules can exceed the native 64 KiB combined-read bound; split the import".into());
            }
        }
    }
    Ok(())
}
